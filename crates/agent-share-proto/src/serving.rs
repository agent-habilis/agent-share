//! Which slots of a share a peer can actually serve, small enough for a card.
//!
//! The availability a `BitTorrent` client paints as a grid, at the granularity
//! this protocol addresses bytes with: a manifest index. A peer publishes what
//! it holds, and every other peer can see who to ask.
//!
//! # Why run-length ranges, and why a cap
//!
//! This rides `PeerCard` on the mesh's **meta** channel, which is a CRDT
//! broadcast to everyone — so its size is everyone's problem, and a frame is
//! capped at 3840 bytes. Two things keep it small:
//!
//! - A complete peer sends `"*"`. One byte, and it covers essentially every
//!   origin plus every finished mirror, which is most peers most of the time.
//! - A partial peer sends sorted ranges, `"0-12,15,40-99"`. A mirror fetches in
//!   manifest order, so its held set is usually one run however many files it
//!   has.
//!
//! Past [`MAX_SERVING_CHARS`] the field is **omitted rather than truncated**. A
//! truncated range list is not a smaller truth, it is a different one — it would
//! read as "I do not have those" for files a peer holds, and send readers to the
//! wrong place. Saying nothing reads as "cannot vouch", which is already
//! handled everywhere.
//!
//! Deliberately *not* per byte-chunk: that changes as every range arrives, and
//! automerge keeps history, so a churning bitfield would grow the shared
//! document without bound and every late joiner would sync all of it.

use std::fmt::Write as _;

/// Ceiling on the encoded field.
///
/// Chosen against the 3840-byte gossip frame with CRDT framing and the rest of
/// the card around it. A 10k-file tree held in one run is six characters; this
/// admits a peer holding hundreds of separate runs before it gives up.
pub const MAX_SERVING_CHARS: usize = 512;

/// The marker for "every live slot".
pub const SERVING_ALL: &str = "*";

/// Prefix marking a base64 bitmap rather than a run list.
///
/// One character, and it cannot be confused with either other form: `*` is a
/// bare star and a run list always begins with a digit.
pub const SERVING_BITMAP: char = '~';

/// The largest tree a bitmap can describe exactly.
///
/// A bitmap is one bit per slot, base64'd — four characters per three bytes —
/// and it spends one character on the [`SERVING_BITMAP`] tag, so the budget is
/// `(MAX_SERVING_CHARS - 1) / 4 * 3` bytes of bits. Forgetting that tag is an
/// off-by-one that only shows up at the exact boundary, which is why
/// `a_bitmap_round_trips_at_every_byte_boundary` tests this value directly.
///
/// Past this a bitmap would have to bucket several slots per bit, which would
/// mean answering "maybe" where every caller currently reads "yes" — a
/// different contract, and not one worth smuggling in here.
pub const MAX_BITMAP_SLOTS: usize = (MAX_SERVING_CHARS - 1) / 4 * 3 * 8;

/// Base64 alphabet, spelled out rather than pulled in as a dependency: this is
/// the only place in the crate that needs one.
const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Encode a bitmap, without padding. The length is implied by `total`.
fn to_base64(bits: &[u8]) -> String {
    let mut out = String::with_capacity(bits.len().div_ceil(3) * 4);
    for group in bits.chunks(3) {
        let mut buffer = [0u8; 3];
        buffer[..group.len()].copy_from_slice(group);
        let packed =
            (u32::from(buffer[0]) << 16) | (u32::from(buffer[1]) << 8) | u32::from(buffer[2]);
        let symbols = group.len() + 1;
        for index in 0..symbols {
            let shift = 18 - index * 6;
            let position = ((packed >> shift) & 0x3f) as usize;
            out.push(BASE64[position] as char);
        }
    }
    out
}

/// Decode what [`to_base64`] wrote. Unknown characters end the run rather than
/// failing the field: one bad byte must cost the tail, not the peer.
fn from_base64(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut packed = 0u32;
    let mut filled = 0u32;
    for symbol in text.bytes() {
        let Some(value) = BASE64.iter().position(|candidate| *candidate == symbol) else {
            break;
        };
        // `position` is an index into a 64-byte table, so it always fits.
        packed = (packed << 6) | u32::try_from(value).unwrap_or(0);
        filled += 6;
        if filled >= 8 {
            filled -= 8;
            // Masked to one byte, so the conversion cannot fail.
            out.push(u8::try_from((packed >> filled) & 0xff).unwrap_or(0));
        }
    }
    out
}

/// Encode the indices a peer can serve.
///
/// `held` need not be sorted or unique. `total` is how many slots the manifest
/// has, so a peer holding all of them can say so in one byte.
///
/// Returns `None` when there is nothing to say (`held` is empty) or when the
/// encoding would exceed [`MAX_SERVING_CHARS`] — both mean *do not publish a
/// field*, which reads as "cannot vouch" rather than as "holds nothing".
///
/// # Panics
/// Never in practice: the `expect("peeked")` inside the run scan consumes an
/// element `peek` has just reported, on an iterator nothing else touches.
#[must_use]
pub fn encode_serving(held: &[u32], total: usize) -> Option<String> {
    if held.is_empty() {
        return None;
    }
    let mut sorted: Vec<u32> = held.to_vec();
    sorted.sort_unstable();
    sorted.dedup();

    if total > 0 && sorted.len() == total {
        return Some(SERVING_ALL.to_owned());
    }

    let mut out = String::new();
    let mut runs = sorted.iter().copied().peekable();
    let mut overflowed = false;
    while let Some(start) = runs.next() {
        let mut end = start;
        while runs.peek().is_some_and(|next| *next == end + 1) {
            end = runs.next().expect("peeked");
        }
        if !out.is_empty() {
            out.push(',');
        }
        if start == end {
            let _ = write!(out, "{start}");
        } else {
            let _ = write!(out, "{start}-{end}");
        }
        // Bail as soon as it is too long rather than building a megabyte first.
        if out.len() > MAX_SERVING_CHARS {
            overflowed = true;
            break;
        }
    }
    if !overflowed {
        return Some(out);
    }

    // The run list does not fit. That is not rare any more: a peer that seeds
    // whatever it happened to look at holds scattered singletons, which is the
    // worst case for runs — and *omitting* the field would make a peer holding
    // a hundred files advertise nothing at all, which is worse than saying it
    // at a coarser grain.
    //
    // A bitmap is one bit per slot however scattered they are, so its size
    // depends on the tree rather than on the shape of the holding.
    encode_bitmap(&sorted, total)
}

/// Encode a held set as a fixed-width bitmap, or `None` if the tree is too
/// large for one bit per slot.
///
/// Exact, never approximate: bit *i* is slot *i*. A coarser bitmap that bucketed
/// several slots per bit would have to mean "maybe", and every caller here reads
/// a set bit as "yes" — so past [`MAX_BITMAP_SLOTS`] this says nothing instead of
/// something it would be wrong to believe.
fn encode_bitmap(sorted: &[u32], total: usize) -> Option<String> {
    if total == 0 || total > MAX_BITMAP_SLOTS {
        return None;
    }
    let mut bits = vec![0u8; total.div_ceil(8)];
    for slot in sorted {
        let slot = *slot as usize;
        if slot >= total {
            continue;
        }
        if let Some(byte) = bits.get_mut(slot / 8) {
            *byte |= 1 << (slot % 8);
        }
    }
    let mut out = String::with_capacity(1 + bits.len().div_ceil(3) * 4);
    out.push(SERVING_BITMAP);
    out.push_str(&to_base64(&bits));
    (out.len() <= MAX_SERVING_CHARS).then_some(out)
}

/// Decode what a peer said it can serve, into sorted indices.
///
/// `total` bounds `"*"`. A malformed run is skipped rather than failing the
/// whole field: one unreadable range must cost that range, not this peer's
/// whole availability.
#[must_use]
pub fn decode_serving(encoded: &str, total: usize) -> Vec<u32> {
    let encoded = encoded.trim();
    if encoded == SERVING_ALL {
        return (0..u32::try_from(total).unwrap_or(u32::MAX)).collect();
    }
    if let Some(body) = encoded.strip_prefix(SERVING_BITMAP) {
        let bits = from_base64(body);
        let mut held = Vec::new();
        for slot in 0..total {
            if bits
                .get(slot / 8)
                .is_some_and(|byte| byte & (1 << (slot % 8)) != 0)
                && let Ok(slot) = u32::try_from(slot)
            {
                held.push(slot);
            }
        }
        return held;
    }
    let mut held = Vec::new();
    for run in encoded.split(',') {
        let run = run.trim();
        if run.is_empty() {
            continue;
        }
        match run.split_once('-') {
            Some((start, end)) => {
                let (Ok(start), Ok(end)) = (start.parse::<u32>(), end.parse::<u32>()) else {
                    continue;
                };
                if start > end {
                    continue;
                }
                held.extend(start..=end);
            }
            None => {
                if let Ok(index) = run.parse::<u32>() {
                    held.push(index);
                }
            }
        }
    }
    held.sort_unstable();
    held.dedup();
    held
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_BITMAP_SLOTS, MAX_SERVING_CHARS, decode_serving, encode_bitmap, encode_serving,
    };

    #[test]
    fn a_complete_peer_is_one_byte() {
        assert_eq!(encode_serving(&[0, 1, 2], 3).as_deref(), Some("*"));
    }

    #[test]
    fn adjacent_indices_collapse_into_a_run() {
        assert_eq!(
            encode_serving(&[0, 1, 2, 3, 7, 9, 10], 20).as_deref(),
            Some("0-3,7,9-10")
        );
    }

    #[test]
    fn holding_nothing_publishes_no_field() {
        assert_eq!(encode_serving(&[], 10), None);
    }

    #[test]
    fn a_round_trip_preserves_the_set() {
        let held = vec![0, 1, 2, 9, 40, 41, 42, 99];
        let encoded = encode_serving(&held, 200).expect("encode");
        assert_eq!(decode_serving(&encoded, 200), held);
    }

    #[test]
    fn a_complete_peer_decodes_to_every_slot() {
        assert_eq!(decode_serving("*", 4), vec![0, 1, 2, 3]);
    }

    /// **Truncation would be a different truth, not a smaller one.** Half a
    /// range list reads as "I do not have those" for files the peer holds, and
    /// sends readers somewhere else. Omitting the field reads as "cannot
    /// vouch", which every caller already handles.
    #[test]
    fn an_oversized_set_is_omitted_rather_than_truncated() {
        // Every other index, so nothing collapses into a run.
        let scattered: Vec<u32> = (0..4000).map(|index| index * 2).collect();
        assert_eq!(
            encode_serving(&scattered, 100_000),
            None,
            "a field that cannot fit must not be published at all"
        );
    }

    #[test]
    fn a_set_that_just_fits_is_published() {
        let modest: Vec<u32> = (0..50).map(|index| index * 2).collect();
        let encoded = encode_serving(&modest, 1000).expect("should fit");
        assert!(encoded.len() <= MAX_SERVING_CHARS);
        assert_eq!(decode_serving(&encoded, 1000), modest);
    }

    /// One unreadable run must cost that run, not the peer.
    #[test]
    fn a_malformed_run_is_skipped_not_fatal() {
        assert_eq!(
            decode_serving("0-2,nonsense,7,9-x,12", 100),
            vec![0, 1, 2, 7, 12]
        );
        // A backwards range is nonsense rather than an empty set.
        assert_eq!(decode_serving("5-1,3", 100), vec![3]);
    }

    /// **The cliff this exists to remove.**
    ///
    /// A peer that seeds whatever it looked at holds scattered singletons. Under
    /// runs alone, ~70-100 of them overflow the frame and the field is dropped —
    /// so a peer holding a hundred files advertises *nothing*, and nobody dials
    /// it. Advertised availability used to rise with holdings and then fall off
    /// a cliff to zero; a bitmap is one bit per slot however scattered, so it
    /// cannot.
    #[test]
    fn a_scattered_holding_is_still_advertised() {
        // Every other slot of a 1000-file tree: 500 runs, far past the cap.
        let scattered: Vec<u32> = (0..500).map(|index| index * 2).collect();
        let encoded = encode_serving(&scattered, 1000).expect("a bitmap must fit");
        assert!(encoded.starts_with(super::SERVING_BITMAP));
        assert!(encoded.len() <= MAX_SERVING_CHARS);
        assert_eq!(decode_serving(&encoded, 1000), scattered);
    }

    /// Holding more must never advertise less. The property the old encoding
    /// broke, asserted directly across the whole range.
    #[test]
    fn advertised_availability_never_falls_as_holdings_grow() {
        let total = 2000;
        let mut previous = 0;
        for count in (1..=1000).step_by(37) {
            let held: Vec<u32> = (0..count).map(|index| index * 2).collect();
            let seen = encode_serving(&held, total)
                .map_or(0, |encoded| decode_serving(&encoded, total).len());
            assert_eq!(seen, held.len(), "a peer must advertise all it holds");
            assert!(seen >= previous, "advertised availability went backwards");
            previous = seen;
        }
    }

    /// The largest bitmap must actually fit, tag included.
    #[test]
    fn the_largest_bitmap_fits_the_frame() {
        let held: Vec<u32> = (0..MAX_BITMAP_SLOTS)
            .step_by(2)
            .map(|slot| u32::try_from(slot).expect("small"))
            .collect();
        let encoded = encode_bitmap(&held, MAX_BITMAP_SLOTS).expect("the cap must be reachable");
        assert!(
            encoded.len() <= MAX_SERVING_CHARS,
            "{} chars",
            encoded.len()
        );
        assert_eq!(decode_serving(&encoded, MAX_BITMAP_SLOTS), held);
    }

    #[test]
    fn a_bitmap_round_trips_at_every_byte_boundary() {
        // Bit-packing bugs hide at the edges of a byte and of a base64 group.
        for total in [1usize, 7, 8, 9, 23, 24, 25, MAX_BITMAP_SLOTS] {
            let held: Vec<u32> = (0..total)
                .filter(|slot| slot % 3 == 0)
                .map(|slot| u32::try_from(slot).expect("small"))
                .collect();
            if held.len() == total {
                continue;
            }
            let encoded = encode_bitmap(&held, total).expect("fits");
            assert_eq!(decode_serving(&encoded, total), held, "total {total}");
        }
    }

    /// A complete peer still says `*`, because one byte beats 512 of them and
    /// it is most peers most of the time.
    #[test]
    fn a_complete_peer_still_prefers_the_star() {
        let held: Vec<u32> = (0..1000).collect();
        assert_eq!(encode_serving(&held, 1000).as_deref(), Some("*"));
    }

    /// A contiguous holding still uses runs: six characters beats a 512-char
    /// bitmap, and a mirror fetching in order is exactly that shape.
    #[test]
    fn a_contiguous_holding_still_prefers_runs() {
        let held: Vec<u32> = (0..500).collect();
        let encoded = encode_serving(&held, 10_000).expect("runs fit");
        assert_eq!(encoded, "0-499");
    }

    /// Past the bitmap's reach the field is still omitted rather than made
    /// approximate. A set bit means "yes" everywhere it is read, so bucketing
    /// slots together would quietly change what every caller believes.
    #[test]
    fn a_tree_too_large_for_a_bitmap_still_says_nothing() {
        let scattered: Vec<u32> = (0..4000).map(|index| index * 2).collect();
        assert_eq!(encode_serving(&scattered, 100_000), None);
    }

    /// A bitmap must never claim a slot outside the tree it describes.
    #[test]
    fn a_bitmap_ignores_slots_past_the_tree() {
        let held = vec![0u32, 5, 99];
        let encoded = encode_bitmap(&held, 10).expect("fits");
        assert_eq!(decode_serving(&encoded, 10), vec![0, 5]);
    }

    /// A corrupted bitmap costs its tail, not the peer — the same tolerance a
    /// malformed run already gets.
    #[test]
    fn a_malformed_bitmap_is_truncated_not_fatal() {
        let held = vec![0u32, 1, 2];
        let encoded = encode_bitmap(&held, 64).expect("fits");
        let mangled = format!("{}!!!", &encoded[..3]);
        // Whatever survives must be a subset of the truth, never a superset.
        for slot in decode_serving(&mangled, 64) {
            assert!(held.contains(&slot), "decoded a slot that was never held");
        }
    }

    #[test]
    fn unsorted_and_duplicated_input_still_encodes_cleanly() {
        assert_eq!(
            encode_serving(&[3, 1, 2, 1, 0], 10).as_deref(),
            Some("0-3"),
            "callers should not have to sort first"
        );
    }
}
