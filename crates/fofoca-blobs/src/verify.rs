//! The bao layer: build an outboard, encode a range, verify a range.
//!
//! Deliberately thin. `bao-tree` does the work; this exists so the rest of the
//! crate names one shape instead of four, and so the block size cannot be
//! passed inconsistently at two call sites — which would produce outboards that
//! silently fail to verify against each other.

use anyhow::{Context, Result};
use bao_tree::io::outboard::PreOrderMemOutboard;
use bao_tree::io::sync::{decode_ranges, encode_ranges_validated, valid_ranges};
use bao_tree::{BaoTree, ChunkRanges};

use crate::BLOCK_SIZE;

/// A BLAKE3 root: the name of some content, and what every range verifies
/// against.
pub type Root = [u8; 32];

/// A pre-order outboard — the hash tree that rides *beside* the data rather
/// than inside it. The reason a store can verify bytes it does not own.
pub type Outboard = Vec<u8>;

/// Hash `data`, returning its root and outboard.
///
/// The one-time cost a caller pays before a file can be served to a third
/// party. Measured at ~2.2 GiB/s natively and ~2.1 GiB/s in wasm with SIMD
/// (RFC 03, S0.3), so roughly half a second per gigabyte — cheap enough to do
/// lazily, on first interest, which is the point.
#[must_use]
pub fn build_outboard(data: &[u8]) -> (Root, Outboard) {
    let outboard = PreOrderMemOutboard::create(data, BLOCK_SIZE);
    (*outboard.root.as_bytes(), outboard.data)
}

/// Encode `ranges` of `data` for a peer, with proofs.
///
/// # Errors
/// The ranges do not fit the data.
pub fn encode_ranges(data: &[u8], ranges: &ChunkRanges) -> Result<Vec<u8>> {
    let outboard = PreOrderMemOutboard::create(data, BLOCK_SIZE);
    let mut encoded = Vec::new();
    encode_ranges_validated(data, &outboard, ranges, &mut encoded)
        .context("encoding verified ranges")?;
    Ok(encoded)
}

/// Verify `encoded` against `root` and write what it proves into `target`.
///
/// Returns the chunk ranges that verified. **A tampered byte fails here**, which
/// is what lets a consumer accept bytes from a peer it has no reason to trust:
/// the worst a hostile peer can do is waste its own bandwidth.
///
/// # Errors
/// The stream does not verify against `root`, or is malformed.
pub fn decode_into(
    root: Root,
    size: u64,
    encoded: &[u8],
    ranges: &ChunkRanges,
    target: &mut Vec<u8>,
) -> Result<ChunkRanges> {
    let tree = BaoTree::new(size, BLOCK_SIZE);
    let mut outboard = PreOrderMemOutboard {
        root: blake3::Hash::from(root),
        tree,
        data: vec![0u8; usize::try_from(tree.outboard_size()).context("outboard too large")?],
    };

    // Size the target to the *whole* file before decoding, even for a window in
    // the middle of it. `decode_ranges` writes at absolute offsets, so a target
    // grown only as far as the window ends leaves the scan below reading past
    // its end — which surfaces as "failed to fill whole buffer" and looks like a
    // verification failure rather than a too-small buffer.
    target.resize(
        usize::try_from(size).context("file too large for this target")?,
        0,
    );
    decode_ranges(
        std::io::Cursor::new(encoded),
        ranges,
        &mut *target,
        &mut outboard,
    )
    .context("verifying ranges against the root")?;

    // Scan only where bytes were expected. Scanning everything would ask about
    // chunks this call never touched, whose zeroes are not a verification
    // failure — they are simply absent.
    //
    // And scan the *answer*, not the request: the two differ when a peer sends
    // part of what was asked for, and believing the request is how a store comes
    // to advertise bytes it does not hold.
    let scan = ranges.clone() & crate::extent_of(size);
    let mut held = ChunkRanges::empty();
    for range in valid_ranges(&outboard, &target[..], &scan) {
        held |= ChunkRanges::from(range.context("scanning verified ranges")?);
    }
    Ok(held)
}

#[cfg(test)]
mod tests {
    use super::{build_outboard, decode_into, encode_ranges};
    use bao_tree::{ChunkNum, ChunkRanges};

    fn data(len: usize) -> Vec<u8> {
        (0..len)
            .map(|index| u8::try_from(index % 251).expect("bounded by 251"))
            .collect()
    }

    #[test]
    fn a_full_round_trip_verifies() {
        let bytes = data(1 << 20);
        let (root, _) = build_outboard(&bytes);
        let all = ChunkRanges::all();
        let encoded = encode_ranges(&bytes, &all).expect("encode");

        let mut target = Vec::new();
        let held =
            decode_into(root, bytes.len() as u64, &encoded, &all, &mut target).expect("decode");
        assert_eq!(target, bytes);
        assert!(!held.is_empty());
    }

    /// The property partial seeding rests on: a peer holding part of a file can
    /// serve that part, provably, against the same root as the whole.
    #[test]
    fn a_partial_range_verifies_against_the_whole_files_root() {
        let bytes = data(1 << 20);
        let (root, _) = build_outboard(&bytes);
        // One 64 KiB chunk group is 64 chunks of 1 KiB.
        let window = ChunkRanges::from(ChunkNum(64)..ChunkNum(128));
        let encoded = encode_ranges(&bytes, &window).expect("encode");

        assert!(
            encoded.len() < bytes.len() / 4,
            "a partial encode must be far smaller than the file: {} of {}",
            encoded.len(),
            bytes.len()
        );

        let mut target = Vec::new();
        let held =
            decode_into(root, bytes.len() as u64, &encoded, &window, &mut target).expect("decode");
        assert!(!held.is_empty());
        assert_eq!(
            &target[64 * 1024..128 * 1024],
            &bytes[64 * 1024..128 * 1024]
        );
    }

    /// The whole trust argument in one test. Without this a peer could serve
    /// anything and be believed.
    #[test]
    fn a_tampered_byte_is_rejected() {
        let bytes = data(1 << 20);
        let (root, _) = build_outboard(&bytes);
        let all = ChunkRanges::all();
        let mut encoded = encode_ranges(&bytes, &all).expect("encode");
        let last = encoded.len() - 1;
        encoded[last] ^= 0x01;

        let mut target = Vec::new();
        assert!(
            decode_into(root, bytes.len() as u64, &encoded, &all, &mut target).is_err(),
            "a flipped bit must not verify"
        );
    }

    /// A root from *different* content must not accept these bytes, even though
    /// they are internally consistent.
    #[test]
    fn bytes_do_not_verify_against_another_files_root() {
        let mine = data(1 << 20);
        let theirs = data((1 << 20) + 1);
        let (their_root, _) = build_outboard(&theirs);
        let all = ChunkRanges::all();
        let encoded = encode_ranges(&mine, &all).expect("encode");

        let mut target = Vec::new();
        assert!(
            decode_into(their_root, mine.len() as u64, &encoded, &all, &mut target).is_err(),
            "content must not verify against an unrelated root"
        );
    }

    #[test]
    fn an_empty_file_has_a_stable_root() {
        let (root, outboard) = build_outboard(&[]);
        assert_eq!(root, build_outboard(&[]).0, "not deterministic");
        assert!(outboard.is_empty(), "nothing to prove about no bytes");
    }

    /// Outboard overhead is the tax on every seedable file, so pin it.
    #[test]
    fn outboard_overhead_stays_under_a_tenth_of_a_percent() {
        let bytes = data(16 << 20);
        let (_, outboard) = build_outboard(&bytes);
        // Integer comparison rather than a float ratio: `outboard * 1000 <
        // data` is exactly "under 0.1%", with no rounding to argue about.
        assert!(
            outboard.len() * 1000 < bytes.len(),
            "64 KiB chunk groups should cost ~0.097%, got {} bytes over {}",
            outboard.len(),
            bytes.len()
        );
    }
}
