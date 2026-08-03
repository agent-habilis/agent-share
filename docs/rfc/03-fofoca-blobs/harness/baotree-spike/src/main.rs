//! S0.3 — native hash/outboard throughput.
//!
//! The question that matters: does `bao-tree`'s outboard construction drive
//! blake3's **wide** multi-chunk SIMD path, or does it hash chunk-by-chunk?
//! Raw `blake3::hash` is the ceiling — it definitely uses the wide path. If
//! outboard construction lands near it, bao-tree is fine. If it lands at a
//! small fraction, the RFC's lazy-hash latency budget is wrong by that factor.

use bao_tree::io::outboard::PreOrderMemOutboard;
use std::time::Instant;

use s01_baotree_wasm::{BLOCK_SIZE_16K, BLOCK_SIZE_64K};

fn bench<F: FnMut()>(label: &str, bytes: usize, iters: u32, mut f: F) -> f64 {
    // Warm once so allocator and caches are not part of the first sample.
    f();
    let mut best = f64::MAX;
    for _ in 0..iters {
        let t0 = Instant::now();
        f();
        best = best.min(t0.elapsed().as_secs_f64());
    }
    let mibs = (bytes as f64 / (1024.0 * 1024.0)) / best;
    println!("  {label:<44} {mibs:>9.1} MiB/s   ({:.1} ms)", best * 1000.0);
    mibs
}

fn main() {
    let mib = std::env::args()
        .nth(1)
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(256);
    let iters = 5;
    let len = mib * 1024 * 1024;
    let data: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();

    println!(
        "\nS0.3 native — {mib} MiB, best of {iters}, {} cores\n",
        std::thread::available_parallelism().map_or(0, std::num::NonZero::get)
    );

    println!("blake3 (the ceiling — known to use the wide SIMD path):");
    let ceiling = bench("blake3::hash single-threaded", len, iters, || {
        std::hint::black_box(blake3::hash(&data));
    });

    #[cfg(feature = "rayon")]
    let rayon = bench("blake3 update_rayon (multithreaded)", len, iters, || {
        let mut h = blake3::Hasher::new();
        h.update_rayon(&data);
        std::hint::black_box(h.finalize());
    });

    println!("\nbao-tree outboard construction (what we actually pay):");
    let ob16 = bench("PreOrderMemOutboard::create @ 16 KiB groups", len, iters, || {
        std::hint::black_box(PreOrderMemOutboard::create(&data, BLOCK_SIZE_16K));
    });
    let ob64 = bench("PreOrderMemOutboard::create @ 64 KiB groups", len, iters, || {
        std::hint::black_box(PreOrderMemOutboard::create(&data, BLOCK_SIZE_64K));
    });

    println!("\nverdict:");
    println!("  outboard@16K / blake3 ceiling = {:.2}x", ob16 / ceiling);
    println!("  outboard@64K / blake3 ceiling = {:.2}x", ob64 / ceiling);
    #[cfg(feature = "rayon")]
    println!("  rayon / single-threaded blake3 = {:.2}x", rayon / ceiling);
    println!(
        "\n  A ratio near 1.0 means bao-tree rides blake3's wide SIMD path.\n  \
         A ratio near 1/8 (~0.12) on AVX2 or 1/4 (~0.25) on NEON means chunk-by-chunk.\n"
    );
}
