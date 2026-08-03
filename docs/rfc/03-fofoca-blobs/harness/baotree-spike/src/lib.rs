//! S0.1 — does `bao-tree` build wasm-clean with the RFC 03 feature set?
//!
//! A build that merely *depends* on the crate proves little: the linker can drop
//! everything. So this exercises the four APIs `fofoca-blobs` would actually
//! need — build an outboard, encode a verified range, decode and verify it, and
//! ask which ranges are valid — forcing those paths to be reachable.

use bao_tree::io::outboard::PreOrderMemOutboard;
use bao_tree::io::sync::{decode_ranges, encode_ranges_validated, valid_ranges};
use bao_tree::{BaoTree, BlockSize, ChunkRanges};

/// 16 KiB chunk groups (2^4 * 1024 B), RFC 01's stated default.
pub const BLOCK_SIZE_16K: BlockSize = BlockSize::from_chunk_log(4);
/// 64 KiB chunk groups — the alternative the RFC leaves unmeasured.
pub const BLOCK_SIZE_64K: BlockSize = BlockSize::from_chunk_log(6);

/// Build an outboard over `data`, returning `(root, outboard_bytes)`.
///
/// This is the "origin hashes one file on demand" path.
pub fn build_outboard(data: &[u8], block_size: BlockSize) -> ([u8; 32], Vec<u8>) {
    let ob = PreOrderMemOutboard::create(data, block_size);
    (*ob.root.as_bytes(), ob.data)
}

/// Encode a verified range slice — the seeder answering a range request.
pub fn encode_range(data: &[u8], ranges: &ChunkRanges, block_size: BlockSize) -> Vec<u8> {
    let ob = PreOrderMemOutboard::create(data, block_size);
    let mut encoded = Vec::new();
    encode_ranges_validated(data, &ob, ranges, &mut encoded).expect("encode");
    encoded
}

/// Decode + verify a range slice against an expected root — the consumer side.
///
/// Returns the decoded bytes and the ranges that verified, which is exactly the
/// `present(root) -> ChunkRanges` bookkeeping `fofoca-blobs` would persist.
pub fn decode_and_verify(
    root: [u8; 32],
    size: u64,
    encoded: &[u8],
    ranges: &ChunkRanges,
    block_size: BlockSize,
) -> (Vec<u8>, ChunkRanges) {
    let tree = BaoTree::new(size, block_size);
    let mut ob = PreOrderMemOutboard {
        root: blake3::Hash::from(root),
        tree,
        data: vec![0u8; tree.outboard_size() as usize],
    };
    let mut decoded = Vec::new();
    decode_ranges(
        std::io::Cursor::new(encoded),
        ranges,
        &mut decoded,
        &mut ob,
    )
    .expect("decode");

    // The `validate` feature: which chunk ranges do we now genuinely hold?
    let mut have = ChunkRanges::empty();
    for r in valid_ranges(&ob, &decoded, &ChunkRanges::all()) {
        have |= ChunkRanges::from(r.expect("valid_ranges"));
    }
    (decoded, have)
}

/// Forces blake3 proper into the binary so its SIMD backend is exercised.
pub fn hash(data: &[u8]) -> [u8; 32] {
    *blake3::hash(data).as_bytes()
}

/// Hand JS a buffer the Rust allocator owns. Writing to an arbitrary offset
/// instead collides with dlmalloc's heap and faults on the first `Vec` growth.
///
/// # Safety
/// Caller must pass the returned pointer and the same `len` back to
/// [`spike_free`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spike_alloc(len: usize) -> *mut u8 {
    let mut v = Vec::<u8>::with_capacity(len);
    let p = v.as_mut_ptr();
    core::mem::forget(v);
    p
}

/// # Safety
/// `ptr`/`len` must come from [`spike_alloc`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spike_free(ptr: *mut u8, len: usize) {
    drop(unsafe { Vec::from_raw_parts(ptr, 0, len) });
}

/// Hash only — the clean number for comparing portable vs SIMD wasm without
/// encode/decode bookkeeping diluting it.
///
/// # Safety
/// `ptr`/`len` must describe a readable buffer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spike_hash_only(ptr: *const u8, len: usize) -> u32 {
    let data = unsafe { core::slice::from_raw_parts(ptr, len) };
    u32::from(hash(data)[0])
}

/// Outboard construction only — what a seeder actually pays before it can
/// serve verified ranges of a file.
///
/// # Safety
/// `ptr`/`len` must describe a readable buffer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spike_outboard_only(ptr: *const u8, len: usize, big: u32) -> u32 {
    let data = unsafe { core::slice::from_raw_parts(ptr, len) };
    let bs = if big == 0 { BLOCK_SIZE_16K } else { BLOCK_SIZE_64K };
    let (_, ob) = build_outboard(data, bs);
    ob.len() as u32
}

/// A cdylib only exports `extern "C"` symbols; everything else is stripped, so
/// a plain `pub fn` proves nothing about what survives into the module. This
/// entry point drags every path above into the binary — without it the wasm
/// output is ~350 bytes of nothing and the import check is vacuous.
///
/// # Safety
/// `ptr`/`len` must describe a readable buffer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spike_exercise_all(ptr: *const u8, len: usize) -> u32 {
    let data = unsafe { core::slice::from_raw_parts(ptr, len) };
    let mut acc = 0u32;

    for bs in [BLOCK_SIZE_16K, BLOCK_SIZE_64K] {
        let (root, outboard) = build_outboard(data, bs);
        acc = acc.wrapping_add(outboard.len() as u32).wrapping_add(root[0] as u32);

        let ranges = ChunkRanges::all();
        let encoded = encode_range(data, &ranges, bs);
        acc = acc.wrapping_add(encoded.len() as u32);

        let (decoded, have) = decode_and_verify(root, len as u64, &encoded, &ranges, bs);
        acc = acc.wrapping_add(decoded.len() as u32);
        acc = acc.wrapping_add(u32::from(!have.is_empty()));
        acc = acc.wrapping_add(u32::from(tamper_is_detected(data, bs)));
    }

    acc.wrapping_add(hash(data)[0] as u32)
}

/// A tampered byte must fail verification. This is the property the whole
/// swarm-trust argument rests on, so prove it here rather than assume it.
pub fn tamper_is_detected(data: &[u8], block_size: BlockSize) -> bool {
    let (root, _) = build_outboard(data, block_size);
    let ranges = ChunkRanges::all();
    let mut encoded = encode_range(data, &ranges, block_size);
    // Flip a bit deep in the payload, past the length prefix and first parent.
    let victim = encoded.len() - 1;
    encoded[victim] ^= 0x01;

    let tree = BaoTree::new(data.len() as u64, block_size);
    let mut ob = PreOrderMemOutboard {
        root: blake3::Hash::from(root),
        tree,
        data: vec![0u8; tree.outboard_size() as usize],
    };
    let mut decoded = Vec::new();
    decode_ranges(
        std::io::Cursor::new(&encoded),
        &ranges,
        &mut decoded,
        &mut ob,
    )
    .is_err()
}
