//! Correctness, not just compilation. A spike that builds but computes the
//! wrong thing answers nothing.

use bao_tree::ChunkRanges;
use bao_tree::ChunkNum;
use s01_baotree_wasm::*;

fn data(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i % 251) as u8).collect()
}

#[test]
fn full_roundtrip_verifies() {
    let d = data(1 << 20); // 1 MiB
    let (root, outboard) = build_outboard(&d, BLOCK_SIZE_16K);
    assert!(!outboard.is_empty(), "outboard should be non-empty for 1 MiB");

    let ranges = ChunkRanges::all();
    let encoded = encode_range(&d, &ranges, BLOCK_SIZE_16K);
    let (decoded, have) = decode_and_verify(root, d.len() as u64, &encoded, &ranges, BLOCK_SIZE_16K);

    assert_eq!(decoded, d, "decoded bytes must match the original");
    assert!(!have.is_empty(), "valid_ranges must report what we hold");
}

/// **The property partial seeding depends on.** A peer holding only part of a
/// file must be able to serve just that part, verifiably.
#[test]
fn partial_range_verifies_against_the_same_root() {
    let d = data(1 << 20);
    let (root, _) = build_outboard(&d, BLOCK_SIZE_16K);

    // Chunks are 1024 B; a 16 KiB block group is 16 chunks. Ask for the
    // second block group only.
    let ranges = ChunkRanges::from(ChunkNum(16)..ChunkNum(32));
    let encoded = encode_range(&d, &ranges, BLOCK_SIZE_16K);

    let (decoded, have) =
        decode_and_verify(root, d.len() as u64, &encoded, &ranges, BLOCK_SIZE_16K);

    assert!(
        encoded.len() < d.len() / 4,
        "a partial encode must be far smaller than the whole file, got {} of {}",
        encoded.len(),
        d.len()
    );
    assert!(!have.is_empty(), "the fetched range must verify");
    // The decoded buffer carries the requested window.
    assert_eq!(&decoded[16 * 1024..32 * 1024], &d[16 * 1024..32 * 1024]);
}

/// The whole swarm-trust argument rests on this.
#[test]
fn a_tampered_byte_is_rejected() {
    let d = data(1 << 20);
    assert!(
        tamper_is_detected(&d, BLOCK_SIZE_16K),
        "a flipped bit must fail verification"
    );
}

/// Outboard overhead: RFC 01 claims ~0.4% at 16 KiB groups. Measure both, since
/// the RFC treats 16 KiB as fixed and it is really a tunable.
#[test]
fn outboard_overhead_is_measured_not_assumed() {
    let d = data(16 << 20); // 16 MiB
    for (label, bs) in [("16KiB", BLOCK_SIZE_16K), ("64KiB", BLOCK_SIZE_64K)] {
        let (_, ob) = build_outboard(&d, bs);
        let pct = (ob.len() as f64 / d.len() as f64) * 100.0;
        println!("chunk group {label}: outboard {} B over {} B = {pct:.4}%", ob.len(), d.len());
    }
}

#[test]
fn hash_matches_blake3() {
    let d = data(4096);
    assert_eq!(hash(&d), *blake3::hash(&d).as_bytes());
}
