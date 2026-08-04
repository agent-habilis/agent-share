# Stage 0 findings

Falsification spikes run **before** any `fofoca-blobs` code, ordered by decision
impact ÷ cost to falsify. The point was to make every load-bearing assumption
fail cheaply if it was going to fail at all.

Each finding below states its verdict, what it would have broken, and what is
still assumed. Raw captures live in [`../data/`](../data/); the design they
inform is in [`../README.md`](../README.md).

Evidence classes, extending the convention in
[`../../02-performance.md`](../../02-performance.md):

- **[measured]** — benchmarked or executed; the number is in `../data/`.
- **[verified]** — confirmed by reading the code directly.
- unmarked — survey-pass belief, re-check before acting on it.

## Status

| # | Assumption | Verdict | Detail |
|---|---|---|---|
| S0.1 | `bao-tree` is wasm-clean | **holds** | [s01](s01-baotree-wasm.md) |
| S0.2 | The mount protocol is symmetric | **holds** | [s02](s02-protocol-symmetry.md) |
| S0.3 | `bao-tree` uses blake3's wide SIMD path | **holds — 4–8× risk falsified** | [s03](s03-hash-throughput.md) |
| S0.4 | Multi-source buys throughput | **holds at K=2 (floor); K=4 unresolved** | [s04](s04-multi-source-throughput.md) |
| S0.5 | OPFS random access works in a Worker | **holds (Safari); Chrome unverified** | [s05](s05-opfs-worker.md) |

**Gate 0 clears.** Nothing found argues against `fofoca-blobs`, and the two
assumptions rated riskiest going in — OPFS and the SIMD path — both came back
better than assumed.

## What remains open

Two items, both narrow, neither blocking Stage 1:

1. **K=4 must be measured before Stage 4 sets the peer-set cap.** The only
   measurement says four sources are *worse* than one. See
   [s04](s04-multi-source-throughput.md).
2. **OPFS in Chrome.** Safari passes; Chrome is untested. See
   [s05](s05-opfs-worker.md).

## What Stage 0 changed about the design

- **64 KiB chunk groups, not 16 KiB.** Four times smaller outboards at
  indistinguishable construction speed (S0.3).
- **One Worker owns both OPFS and hashing.** The Worker-only constraint on sync
  access handles stopped being a tax once hashing had to leave the main thread
  anyway (S0.5).
- **No need to drive blake3's guts.** `bao-tree` already rides the wide SIMD
  path; the feared 8× penalty does not exist (S0.3).
- **RFC 01's throughput mechanism is misattributed** and needs re-deriving —
  which is what [`../../02-performance.md`](../../02-performance.md) asked for
  (S0.4).

## Method notes worth keeping

Two mistakes were made and caught during Stage 0. Both are the kind that produce
confident, wrong answers:

- **A dependency-only wasm build proves nothing.** The first cdylib was 367
  bytes: nothing was `extern "C"`, so the linker stripped every code path and
  the import check passed vacuously. Forcing an exported entry point that
  exercises the real APIs took it to 122 KB.
- **Profile matters more than the thing being measured.** The first S0.4 run was
  a debug build and reported the *opposite* scaling direction from release.
  Any throughput number from `cargo test` without `--release` is noise.

A third, structural: a single pass through a parameter sweep confounds the
effect with order and warm-up, and gives no variance estimate. Interleaved
repetition turned S0.4 from "spread 2.0×, effect 1.82×" (unreadable) into
"spread 1.07×, effect 1.61×" (readable).
