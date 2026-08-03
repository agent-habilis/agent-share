# S0.4 — does multi-source actually buy throughput?

**Verdict: holds at K=2 as a floor; K=4 unresolved and gating.** Raw capture:
[`../data/s04-multi-source.txt`](../data/s04-multi-source.txt). Harness:
`mount::bench::tests::s04_multi_source_throughput_scaling` (`#[ignore]`d).

## Why it mattered

This was the highest-stakes experiment in Stage 0. RFC 01 justifies swarming
partly on throughput; if a second source bought nothing, that motivation
collapses to resilience and fan-out only, and `fofoca-blobs` is probably
unjustified.

It also needed no new protocol — `mount/bench.rs` and `OP_BENCH` already
existed — which is why it ran before any crate code.

## What was established

**[measured]** Aggregate throughput, `WebRTC` on loopback, 3 reps × 5 s,
interleaved:

| K | median MiB/s | spread | vs K=1 |
|---|---|---|---|
| 1 | 29.4 | 1.07× | 1.00× |
| 2 | 47.2 | 1.06× | **1.61×** |
| 4 | 17.9 | 1.12× | 0.61× |

Direction is consistent across all three runs performed (debug 4.24×, release
1.82×, improved release 1.61×); only magnitude varies.

The structural argument: **if the ceiling were shared** (CPU, memory bandwidth),
K=1 would already have reached the K=2 aggregate. It did not — 29.4 vs 47.2 —
so a per-connection limit exists and a second connection recruits capacity past
it.

## Why 1.61× is a floor, not an estimate

Measured at 0.3 ms RTT, where no flow-control window can bind: the measured rate
implies a **~9 KiB** effective window, not the 128 KiB RFC 01 cites. That is the
condition *least* favourable to the multi-source hypothesis, and it still won.

Adding RTT can only help:

- Per-connection window limits bind **harder** as RTT grows (rate ≤ W/RTT).
- Slower per-connection rates leave **more** headroom below the shared CPU
  ceiling.

Co-location makes it a floor twice over — real sources sit on independent
network paths. The one mechanism that could flip the sign, N connections
congesting a shared bottleneck link, requires exactly the topology a swarm does
not have.

## The correction this forced to RFC 01

**[verified]** RFC 01 attributes the ceiling to *"SCTP's 128 KiB receive
window — ~18 MB/s at 0 ms RTT collapsing to <2 MB/s at 50 ms"*. That is the
textbook result for a **reliable, ordered** channel, where the sender stalls
waiting on retransmits.

This transport negotiates the data channel **unreliable and unordered** —
`ordered: false, Reliability::MaxRetransmits { retransmits: 0 }`
(`fofoca-iroh-webrtc-transport/src/host/jsep.rs:104-106`), documented at
`lib.rs:4-7`: *"QUIC above it owns loss recovery and congestion control, so
reliable ordered SCTP underneath would stack a second retransmission loop."*

So that stall is **not** the mechanism here. The conclusion survives — a second
source is worth ≥1.61×, measured — but the stated cause, and any figure derived
from 128 KiB/RTT, should be dropped.

This is precisely the re-derivation
[`../../02-performance.md`](../../02-performance.md) asked for when it noted the
claim "now has no backing and should be re-derived, not inherited". Related:
RFC 01's two citations of `docs/research/iroh-webrtc/` are dangling, and that
directory was deleted as misleading.

Supporting detail: `BUFFER_CAP = 1 MiB` exists only in the **browser** backend
(`web/transport.rs:34`), and the repo sets **no quinn `TransportConfig`**
anywhere, so the host path runs on quinn defaults.

## What is still open, and what it gates

**K=4 measures worse than K=1 (0.61×) and this is not explained.** The likely
cause is CPU contention at ~29 MiB/s per connection, which should vanish at
~2 MB/s per connection — but that is a hypothesis.

It matters because **RFC 01 caps the source set at ~4**. Sizing the scheduler on
an untested belief is the failure mode Stage 0 exists to prevent, so this must
be measured **before Stage 4 sets that cap**. It does not block Stage 1, which
rests on resilience rather than throughput.

## Caveats that must travel with these numbers

1. **The bench is serial.** Per
   [`../../02-performance.md`](../../02-performance.md) finding #1
   **[verified]**, `fill_once` is awaited one at a time and opens its own
   bi-stream each time (`mount/bench.rs:493`). So K=1 is a **single-stream
   serial** number, not a saturated connection, and part of the K=2 gain may be
   pipelining rather than genuine multi-source capacity. The bench structurally
   cannot separate the two. This is the weakest point in the finding.
2. **All peers share one host and one CPU.** Contention is a confound at higher
   K regardless of RTT.
3. **RTT is ~0**, so the per-connection-ceiling claim is untested directly — the
   floor argument above is what carries the conclusion, not the measurement.

## Method notes

- **The first run was a debug build** and reported the *opposite* scaling
  direction (K=2 = 4.24×) from release (0.42×). Any throughput number from
  `cargo test` without `--release` is noise.
- **A single pass through the sweep is unreadable.** Run 2 gave effect 1.82×
  against spread 2.0× — noise as large as the signal. Interleaved repetition
  gave effect 1.61× against spread 1.07×.
- **The harness now reports measured RTT and the implied per-connection
  window**, which makes it self-validating: a shaped run where `pfctl` silently
  failed shows RTT ≈ 0 and is discarded rather than misread.
