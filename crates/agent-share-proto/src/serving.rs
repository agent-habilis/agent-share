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
            return None;
        }
    }
    Some(out)
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
    use super::{MAX_SERVING_CHARS, decode_serving, encode_serving};

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

    #[test]
    fn unsorted_and_duplicated_input_still_encodes_cleanly() {
        assert_eq!(
            encode_serving(&[3, 1, 2, 1, 0], 10).as_deref(),
            Some("0-3"),
            "callers should not have to sort first"
        );
    }
}
