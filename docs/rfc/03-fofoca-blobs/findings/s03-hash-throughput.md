# S0.3 — hashing and outboard-construction throughput

**Verdict: holds; the headline risk is falsified.** Raw capture:
[`../data/s03-hash-throughput.txt`](../data/s03-hash-throughput.txt).

## Why it mattered

Two questions, one of them worth up to 8×:

1. Does `bao-tree`'s outboard construction drive blake3's **wide** multi-chunk
   SIMD path, or does it hash chunk-by-chunk? Chunk-by-chunk would cost 4× on
   NEON and 8× on AVX2, and would blow the lazy-hash latency budget.
2. Is wasm `simd128` a real win? The browser has no native BLAKE3 at all —
   `crypto.subtle` does not offer it — so unlike SHA-256 there is no free fast
   path, and wasm is the only implementation.

## What was established

**[measured]** `bao-tree` rides the wide SIMD path. Outboard construction is
**0.95× of raw blake3** (2240 vs 2359 MiB/s native). The 8× risk does not
exist, and there is no need to drive blake3's guts ourselves.

**[measured]** `simd128` is worth **1.80× on hashing, 1.72× on outboard
construction** in wasm, taking the browser to **92% of native single-threaded**
(2160 vs 2359 MiB/s). Build cost: +12 KB.

**[verified]** `rayon` does **not** apply to outboard construction. blake3's
`rayon` feature only adds `update_rayon` / `update_mmap_rayon` — its
*whole-input* APIs — and `bao-tree` walks the tree itself. The 5.34× multicore
figure is real for raw hashing and unavailable to us. **2.2 GiB/s
single-threaded is the rate that applies**, or ~0.45 s per GiB, which is ample
for hashing lazily on demand.

## Chunk group size: 64 KiB, not 16 KiB

RFC 01 fixes 16 KiB chunk groups and treats it as a constant. Measured:

| Chunk group | Outboard overhead | Construction speed |
|---|---|---|
| 16 KiB | 0.3902 % | 0.95× of ceiling |
| 64 KiB | **0.0973 %** | 0.96× of ceiling |

RFC 01's "~0.4 %" is confirmed at 16 KiB. But **64 KiB is 4× smaller for
free** — construction speed is indistinguishable — and it aligns better with the
kernel's `rsize=131072` NFS reads. The cost is coarser partial-seed granularity
and more bytes discarded per verification failure.

## Two prior estimates this corrects

- **My own pre-measurement estimate** of 200–400 MiB/s for portable wasm blake3
  was wrong by about 4×; it is 1197 MiB/s. Labelled an estimate at the time,
  which is why it did no damage — but it shows how far off intuition was.
- **[`../../02-performance.md`](../../02-performance.md)** cites "6× on large
  inputs under Wasmtime" for `blake3/wasm32_simd`, and rates the lever *modest*
  because "blake3 is iroh's hashing and is not obviously on our per-byte path".
  Both need revisiting: the speedup is **1.80× under V8**, not 6× (engine choice
  evidently matters a great deal), and under RFC 03 blake3 moves **directly onto
  the per-byte path**, since every non-origin byte is bao-verified.

## The scoping point that matters most

SIMD does **not** buy streaming throughput — the link is the bottleneck by more
than an order of magnitude. What it buys is **outboard construction latency**:
the one-time, per-file cost that blocks the first swarm fetch of a file, which a
user waits on directly.

Nobody should read this finding as a reason to do SIMD work to make transfers
faster.

## Still assumed

- **Measured under node/V8, not a browser engine.** Safari's
  JavaScriptCore may differ, and the 6×-vs-1.8× gap above is evidence that
  engines diverge a lot here. Worth re-running in-browser alongside the S0.5
  harness.
- Native figures are aarch64/NEON only. x86-64 with AVX-512 is untested; the
  wide-path conclusion should hold but the ratios will differ.
