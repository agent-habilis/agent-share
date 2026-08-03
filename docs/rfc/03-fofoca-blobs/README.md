# RFC 03: `fofoca-blobs`, a storage-agnostic verified byte store

Status: **draft** — design agreed with the user; **Stage 0 executed**, all five
assumptions tested. No crate code written.

Scope decided with the user: chunk/range serving *and* content-addressed
identity are both in scope; **browser seeding is a hard requirement**. Those two
together disqualify `iroh-blobs` and motivate a small crate of our own.

## Layout

This RFC is a folder because it carries measurements, and measurements outlive
prose. RFCs [01](../01-every-peer-a-seeder.md) and [02](../02-performance.md)
are single files because they carry none.

| Path | Holds |
|---|---|
| `README.md` (this file) | The design and the decisions. Prose only. |
| [`findings/`](findings/) | One write-up per Stage 0 spike: verdict, what it would have broken, what is still assumed. |
| [`data/`](data/) | Raw captures — benchmark output, dependency trees, browser results. Regenerable; cite these, do not retype them. |
| [`harness/`](harness/) | The spike code that produced `data/`, kept so the numbers can be re-derived and the two open verifications can be run. |

**Numbers belong in `data/`, judgement belongs in `findings/`, design belongs
here.** Where a figure appears in this file it is a rounded summary with a link,
never the source of truth.

Evidence classes extend the convention in [02](../02-performance.md):
**[measured]** (benchmarked here), **[verified]** (read the code directly),
unmarked (survey belief — re-check before acting on it).

## Context: why RFC 01 Phase 4 is not sufficient

[RFC 01](../01-every-peer-a-seeder.md) turns a share from a star into a swarm,
and its Phase 4 adds `bao-tree` outboards for verified range fetches. That
delivers chunk serving. It deliberately does **not** deliver identity: its "What
this still cannot do" section rules out identity across restarts, resumable
downloads, and cross-share dedup — all downstream of per-serve index addressing.

Two decisions extend it:

1. **Content-addressed identity is now in scope**, alongside chunk serving.
2. **Browser seeding is a hard requirement.** Per RFC 01's own table, the
   browser mirror (`web/src/mount.ts::syncMount`) is the only consumer shape
   holding readable, enumerable bytes. It *is* the seeder.

`iroh-blobs` is the obvious answer to (1) and is disqualified by (2): its wasm
store is `MemStore`-only (n0-computer/iroh-blobs#84, **verified open** — the
issue describes storage abstraction as future work), so browser seeding would
mean holding an entire share in JS heap. RFC 01 rejects `iroh-blobs` for a
different and weaker reason (eager hashing); the decisive one is the store.

But iroh-blobs' *only* real blocker for us is that its store is not abstracted.
Everything else we need from it is `bao-tree`, which is already wasm-clean
([S0.1](findings/s01-baotree-wasm.md)). So: **build the storage seam iroh-blobs
lacks, and nothing else.**

## The load-bearing design inversion

Everything below follows from one sentence, and it is why this cannot be a fork
of iroh-blobs — it is an opposite ownership model, not a missing feature.

> **iroh-blobs owns its data. `fofoca-blobs` must not.**

iroh-blobs takes bytes and stores them under a hash. That is why serving
requires a full read, why every editor save re-hashes, and why browsing a 500 GB
share while reading three files is impossible under it.

In `fofoca-blobs`, natively, the blob **is the user's file, in place**. The store
owns only sidecar metadata — outboard, root binding, range bitfield — for a file
it never copies. This preserves `manifest.rs:18-20`'s refusal to hash up-front
and keeps `mount/produce.rs::serve` an instant `stat` walk.

Second first-class concept, which iroh-blobs has no room for:

> **A root is bound to a version: `(size, mtime) -> root`.**

Mutable files are a supported case, not a violation. This collapses RFC 01's
guard #1 (tree generation) and guard #3 (size/mtime gate) into a single
invariant rather than two independently-patchable checks.
[S0.2](findings/s02-protocol-symmetry.md) demonstrates the exact failure both
guards exist to prevent.

## Design

### What the crate owns

- **`trait BlobStore`** — the seam. ~6 methods, all futures `?Send` so one trait
  serves tokio and wasm: `present(root) -> ChunkRanges`, `read_ranges`,
  `write_verified`, `outboard`, `put_outboard`,
  `bind(key, size, mtime) -> Option<root>`.
- **Backends** — `FsStore` (native; sidecar outboard cache, data read in place),
  `OpfsStore` (browser), `MemStore` (tests).
- **Persisted `ChunkRanges` per root** — one structure serving three jobs:
  resume after restart, partial-seed advertisement, and "what can I serve".
- **The versioned root binding**, per the inversion above.
- **Verification** — a thin wrapper over `bao_tree::io::{decode_ranges,
  encode_ranges_validated}` plus `CreateOutboard` for lazy construction.

### What the crate deliberately does not own

This list is the discipline that keeps it ~2k lines instead of iroh-blobs'
~15k. It is a constraint, not a to-do:

- No transport, ALPN, or framing. Callers move bytes; the store verifies them.
- No discovery — that stays on gossip / `PeerCard`.
- **No Downloader or peer scheduling.** That stays in RFC 01 Phase 5's
  `mount/sources.rs`, because it needs `MAX_DIRECT_PEERS`
  (`agent-habilis-mesh/src/transport/webrtc.rs:451`), the shared ICE budget, and
  `PeerCard.transport` ranking. It is not generic, and making it generic is
  exactly how iroh-blobs gets rebuilt by accident.
- No GC, no tags, no collections/HashSeq. `MountManifest` is the collection.

### Placement and dependencies

`crates/fofoca-blobs/`, a normal workspace member, consumed by the standalone
`crates/agent-share-wasm-client` workspace **by path** — the pattern
`agent-share-proto`, `agent-habilis-mesh` and `fofoca-iroh-webrtc-transport`
already use (`agent-share-wasm-client/Cargo.toml:26-32`).

The `fofoca-` prefix follows `fofoca-iroh-webrtc-transport` and marks the crate
ours-and-publishable. Note the tension it carries: **"blobs" inherits
iroh-blobs' vocabulary while the design inverts its central assumption**, so the
ownership inversion must be stated in the crate's own module docs rather than
left to the name.

`bao-tree` 0.16 and `blake3` 1.8.5 (already transitive, `Cargo.lock:551`).
Neither is iroh-adjacent, so **neither needs a `[patch.crates-io]` entry** —
sidestepping RFC 01 risk #6, where the wasm client's duplicated patch table must
move in lockstep with the root. `iroh-blobs` would have needed one. Exact
feature recipe and the full dependency set:
[S0.1](findings/s01-baotree-wasm.md).

## Availability: which plane answers which question

RFC 01 says availability needs "no request/response protocol". True for *file*
-level availability, and it correctly bans per-chunk state from the CRDT
(automerge retains history; a churning bitfield grows the meta doc without bound
and every late joiner syncs all of it). But it leaves *chunk*-level availability
unanswered, and Stage 4 needs it.

| Plane | Mechanism | Cost | Right job |
|---|---|---|---|
| Automerge `Meta` (`PeerCard`) | replicated, read locally | 0 RTT, **permanent history** | file-level `serving` + `tree` |
| **Directed App frame** (`to: Some`, `corr: Some`) | **unicast** over an existing mesh link | 1 RTT, **no ICE**, ≤3840 B | **chunk availability, pre-connect** |
| Broadcast App frame (`to: None`) | gossip fan-out | O(N) mesh traffic, ≤3840 B | rare "who has root H?" once the origin is dead |
| `MOUNT_ALPN` stream | direct QUIC/`WebRTC` | a JSEP round, 20 s deadline, 1 of 16 slots | bytes, plus `OP_HAVE` once connected |

**The constraint that decides it:** RFC 01 forbids opening a session for a fetch
below ~16 MiB because ICE dominates, and `MAX_DIRECT_PEERS = 16` is shared with
the mesh. So a peer must be able to ask "do you have these chunks?" *before*
spending an ICE slot — which rules out `MOUNT_ALPN` for the pre-connect
question, since reaching it is the expensive decision being made.

The directed App frame fits exactly. **[verified]**
`AppFrameParams { tag, to, corr, body }`
(`agent-habilis-mesh/src/protocol/message/mod.rs:518-523`); `deliver` routes
`to == Some` as **unicast, not gossip** (`gossip/broadcast.rs:22-24`), so it
costs no broadcast traffic; and `corr` gives request/response via a parked
waiter, named as a supported pattern at `gossip/app.rs:16`. **This is what
`ShareDriver::on_app_frame` returning `false` has been reserving** — RFC 01
records the plane as "entirely unused" without ever saying what it is for.

So two tiers, introduced in Stage 4, not Stage 1:

- **Pre-connect (source selection)** — directed `HAVE?{index}` → `HAVE{ranges}`,
  RLE, capped at 3840 B. Advisory: answer coarser if it does not fit.
- **Post-connect (scheduling)** — `OP_HAVE` on the open `MOUNT_ALPN` stream,
  unbounded, refreshed as the peer acquires chunks. This is
  `BlobStore::present(root) -> ChunkRanges` surfaced on the wire.

**Neither is needed in v1.** Least-outstanding dispatch discovers availability
implicitly — ask, get `BadIndex` or a short read, demote. With ~4 peers that is
cheap and needs zero new protocol. Chunk queries earn their place only when
peers hold *disjoint* chunk subsets of one large file; `"*"` covers ~100% of
whole-tree seeders and `syncMount` writes in manifest order, so partial mirrors
are prefix runs. Recorded as a deliberate deferral with a named trigger.

Two further notes. **[verified]** `send_app` refusing an oversized frame is
accurate, but the engine *does* reassemble multipart bodies (`surface_logical`,
`MAX_LOGICAL_BODY_BYTES`); sharding is "entangled with the application's
per-author hash chain" (`broadcast.rs:32-34`), so it is unreachable for us
rather than nonexistent. And directed frames ride **plaintext**, Ed25519-signed
only (`broadcast.rs:37-40`), so a `HAVE?` carrying a content hash is visible to
its addressee — consistent with RFC 01 risk #5 (ticket holders already hold full
read capability), but worth stating rather than discovering.

## Performance

Full figures: [S0.3](findings/s03-hash-throughput.md); raw capture
[`data/s03-hash-throughput.txt`](data/s03-hash-throughput.txt).

**Scope it honestly first.** SIMD does **not** buy streaming throughput — the
link is the bottleneck by more than an order of magnitude. What it buys is
**outboard construction latency**: the one-time, per-file cost that blocks the
first swarm fetch of a file and that a user waits on directly. Nobody should
read this section as a reason to do SIMD work to make transfers faster.

Four design consequences:

1. **`bao-tree` rides blake3's wide SIMD path** — outboard construction is 0.95×
   of raw blake3. No need to drive blake3's guts. The 4–8× risk is falsified.
2. **`rayon` does not apply** to outboard construction, only to blake3's
   whole-input APIs. ~2.2 GiB/s single-threaded is the real rate — ample for
   lazy on-demand hashing at ~0.45 s per GiB.
3. **Use 64 KiB chunk groups, not RFC 01's 16 KiB.** Four times smaller
   outboards (0.097 % vs 0.390 %) at indistinguishable construction speed, and
   better aligned with the kernel's `rsize=131072` NFS reads. The cost is
   coarser partial-seed granularity and more bytes discarded per verification
   failure.
4. **Enable `blake3/wasm32_simd` with `-C target-feature=+simd128`** in
   `.cargo/config.toml:12-13` **and** the wasm client's own copy — the same
   lockstep footgun as the patch table. Worth ~1.8×, costs +12 KB. This matters
   more in the browser than natively because **a browser has no native BLAKE3 at
   all**; `crypto.subtle` does not offer it, so wasm is the only implementation
   and SIMD is the only lever.

`+simd128` applies to the whole bundle, setting a floor of Safari 16.4 /
Chrome 91 / Firefox 89. **Recommendation: accept and document the floor** rather
than shipping dual binaries with feature detection. This agrees with
[02](../02-performance.md)'s SIMD section, though not with its 6× figure — see
[S0.3](findings/s03-hash-throughput.md).

**The Worker convergence.** `FileSystemSyncAccessHandle` is Worker-only, and
hashing a multi-gigabyte mirror would jank the main thread anyway. **One
dedicated Worker owns both OPFS handles and all hashing.** A listed risk turned
into an asset; design the browser backend around it from the start. OPFS is not
the bottleneck — ~1 GiB/s writes against ~2 GiB/s hashing
([S0.5](findings/s05-opfs-worker.md)).

## Phases

Stage 0 was a set of falsification spikes run before any crate code, ordered by
decision impact ÷ cost to falsify. **All five ran; Gate 0 clears.** Verdicts and
what each would have broken: [`findings/`](findings/).

Artifacts that persist from Stage 0:
`mount::tests::a_non_origin_peer_serves_the_origins_ticket_secret` and
`::a_diverged_peer_answers_plausibly_and_wrongly` (permanent regression guards),
and `mount::bench::tests::s04_multi_source_throughput_scaling` (`#[ignore]`d
measurement).

### Stage 1 — RFC 01 Phases 0–3, unchanged

Ships before any of `fofoca-blobs`; needs no hashing and no content addressing.

- **Established by Stage 0:** the protocol is symmetric, so a re-seeder needs no
  new auth code ([S0.2](findings/s02-protocol-symmetry.md)). The resilience
  argument — the main motivation for these phases — is confirmed.
- **Still assumed:** the browser mirror handle is genuinely re-servable; a
  re-seeder serving the origin's manifest verbatim keeps index authority
  coherent under `OP_WATCH` deltas; ~4 swarm sessions do not starve
  `MAX_DIRECT_PEERS = 16`.
- **Kill-gate:** RFC 01's headline test — produce from the CLI, mirror in tab A,
  kill the producer, confirm tab B still mounts and reads the full tree from A.
- **Natural stopping point.** Phase 3 alone delivers the headline capability.

### Stage 2 — `fofoca-blobs`, phased by backend

The trait exists to make the conformance suite write-once. Write it at 2a and
re-run it unchanged per backend.

- **2a — trait + `MemStore`.** *Kill-gate:* if the trait shape cannot serve
  tokio and wasm without contortion, find out here, before two backends depend
  on it.
- **2b — `FsStore`.** *Kill-gate:* the don't-own-the-data inversion holds
  against a real mutable filesystem — a file changing mid-outboard is detected,
  never silently mis-served.
- **2c — `OpfsStore`.** De-risked by [S0.5](findings/s05-opfs-worker.md); same
  suite.
- **Established by Stage 0:** the wasm toolchain works end to end, OPFS random
  access works and persists, `web-sys` exposes the bindings.
- **Still assumed:** that a single `?Send` trait serves both runtimes without
  painfully infecting every signature. That is 2a's whole point.

### Stage 3 — wire it in (RFC 01 Phase 4, retargeted)

`OP_HASH` (op `5`; `OP_BENCH = 4` per `framing.rs`), origin hashes on demand,
consumers verify every range from a non-origin source. Hashes become
**persisted** rather than ephemeral — that is what unlocks identity.

- **Established by Stage 0:** verification works and rejects tampering; partial
  ranges verify against the same root; hashing is fast enough to stay lazy.
- **Kill-gate:** a tampered byte from a non-origin peer fails verification and
  bans that peer; a file with no hash yet still falls back to the origin.
- **Do not touch `MountManifest::encode`** — an op an older producer does not
  know costs one stream; a changed manifest encoding breaks every issued ticket.

### Stage 4 — multi-source reads (RFC 01 Phase 5)

Whole-file paths first (`web/src/download.ts`, a new `agent-share get`), **NFS
last**. `sources.rs` consumes `BlobStore` but still owns peer selection.

- **Established by Stage 0:** a second source is worth ≥1.61×, as a floor
  ([S0.4](findings/s04-multi-source-throughput.md)).
- **GATED — measure K=4 before setting the peer-set cap.** RFC 01 caps the
  source set at ~4, and the only measurement says four sources are *worse* than
  one. Sizing the scheduler on an untested belief is the failure mode Stage 0
  exists to prevent.

### Stage 5 — measure at K seeders (RFC 01 Phase 6)

Confirmation rather than discovery, since S0.4 answered the core question early.
Note that per [02](../02-performance.md), `agent-share bench` measures a serial
single-stream path and cannot see concurrent-read behaviour; a K-seeder scenario
inherits that limitation and should be read alongside a real
`serve` → mount → `cp` of a large file.

## Corrections to RFC 01

**All six are applied**, marked inline in
[RFC 01](../01-every-peer-a-seeder.md) as `CORRECTED` / `NARROWED` blocks that
quote the original text rather than replacing it silently — each was
load-bearing somewhere, and a reader who remembers the old claim needs to find
out it moved. RFC 01's load-bearing facts now carry evidence classes
(11 of 11 bullets). [RFC 02](../02-performance.md) is amended too: its "nothing
here has been measured" opener, its unbacked-ceiling note, and its SIMD
section's 6× figure.

Summary of what changed:

1. **The throughput mechanism is misattributed.** RFC 01 attributes the ceiling
   to SCTP's 128 KiB receive window, which is the textbook result for a
   *reliable, ordered* channel. **[verified]** This transport negotiates the
   channel **unreliable and unordered** precisely so QUIC above owns loss
   recovery, so that stall is not the mechanism here. The conclusion survives;
   the stated cause and any figure derived from 128 KiB/RTT should be dropped.
   Detail: [S0.4](findings/s04-multi-source-throughput.md).
2. **Two citations are dangling.** RFC 01 cites
   `docs/research/iroh-webrtc/README.md` (line 20) and
   `.../bench/RESULTS.md` (line 379). That directory was deleted as misleading,
   and [02](../02-performance.md) says it "should not be cited and should not be
   restored from git history as evidence".
3. **The `iroh-blobs` rejection is right for a weaker reason than the real one.**
   Keep it; the decisive fact is the wasm `MemStore` against a hard
   browser-seeding requirement, not the eager-hashing cost.
4. **`bao-tree`'s wasm-clean recipe** is `default-features = false` plus
   re-adding, not "minus its `fs` feature" — `tokio_fsm` pulls `iroh-io`.
5. **`on_app_frame` has a purpose** — the reserved socket for pre-connect
   `HAVE?` queries. Also correct the `send_app` sharding note per
   `broadcast.rs:32-34`.
6. **Chunk group size is a tunable, not a constant** — see Performance.

## Edge cases and risks

1. **OPFS in Chrome is unverified.** Safari 27 passes; Chrome is untested. Sync
   handles are Worker-only in both, but quota policy and eviction differ.
2. **OPFS eviction.** A seeder that silently loses ranges it advertises is a
   correctness problem, not a performance one. `navigator.storage.persist()` is
   the mitigation and is untested.
3. **A `?Send` async trait across tokio and wasm** infects every signature.
   Known-annoying, solved pattern, but it is 2a's kill-gate for a reason.
4. **Partial-outboard crash consistency is ours.** `bao-tree` supplies
   primitives, not bookkeeping. A persisted range bitfield that outlives its
   data means claiming to hold bytes we do not — a silent-corruption class.
5. **A file mutating mid-download invalidates its outboard.** The store
   *detects* this via the version binding; the *recovery* policy (fall back to
   the origin) belongs to `agent-share`, not the crate.
6. **Honest sizing.** ~3 weeks, against ~1 for RFC 01 Phase 4 as written. Phase
   4 was already buying OPFS, outboard caching and range verification; the
   marginal cost is the trait seam, the persisted bitfield and the version
   binding.

## Rejected alternatives

- **`iroh-blobs` as the byte plane.** See Context. Revisit only if
  n0-computer/iroh-blobs#84 lands a persistent wasm store.
- **Forking `iroh-blobs` and swapping its store.** The ownership model is
  inverted, not incomplete: it wants to own the bytes, and the whole point here
  is verifying files we do not own, in place.
- **Per-chunk availability in the CRDT.** Automerge retains history; a churning
  bitfield grows the meta doc without bound. RFC 01 already rejects this.
- **A generic Downloader inside the crate.** It needs `MAX_DIRECT_PEERS`, the
  ICE budget and `PeerCard.transport`. Generalising it is how iroh-blobs gets
  rebuilt by accident.
- **Injecting link delay inside the transport** to avoid needing root for S0.4.
  The only chokepoint (`host/driver.rs:296-301`) would need a spawned task per
  datagram; the reordering and scheduler noise would confound the measurement.
  Modifying production transport code to make a benchmark work is the wrong
  trade.

## What this still cannot do

- **The manifest stays origin-authoritative.** Individual *files* gain durable
  identity; the *tree* does not. A share can survive its origin's death frozen
  at the last manifest every peer saw, but it cannot outlive its origin and keep
  mutating.
- **Indices remain a per-serve invariant.** Content addressing gives files
  identity across restarts; it does not make READ addresses stable.
- **Lazy mounts consume from the swarm but never join it** without an explicit
  `mirror`. Unchanged from RFC 01.
- **No cross-origin sharing in the browser.** OPFS is origin-scoped, so a mirror
  seeded on one origin is invisible to another.

## Verification

Per-spike reproduction steps live with each finding. The two open items:

**S0.4, delayed link** — `dnctl pipe 1 config delay 25ms` plus a
`dummynet out on lo0 proto udp` rule, then
`S04_REPS=5 S04_SECS=10 cargo test --release -p agent-share --lib s04_ -- --ignored --nocapture`.
**Read the "measured median RTT" line first** — if it is ~0 the shaping never
reached the test's traffic and the numbers are meaningless. Restore pf
afterwards. There is a real chance macOS dummynet does not apply to `lo0`, in
which case the honest next step is two physical machines.

**S0.5 in Chrome** — serve the OPFS harness over `http://localhost` (a secure
context), run it, then **reload and run again**; the second run must report
persistence.

**Stage 2 onward** — the `BlobStore` conformance suite, written once at 2a and
re-run per backend, is the verification. Add a wasm build assertion that no
`import "env"` survives, the same check iroh-blobs' own wasm CI makes and the
one [S0.1](findings/s01-baotree-wasm.md) already performs.
