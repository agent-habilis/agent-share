# RFC 02: where the time actually goes

Status: **draft** — research only, no work started. Every item is a hypothesis
with a named mechanism and a named falsifier.

~~Nothing here has been measured.~~ **Two items now have numbers**, gathered
while validating [RFC 03](03-fofoca-blobs/README.md) rather than by working
through this document's phases:

- **`blake3/wasm32_simd`** — measured at 1.80×, not the 6× cited below, and its
  "modest" rating flips if RFC 03 lands. See the SIMD section.
- **Whether a second connection adds throughput** — yes, ≥1.61×
  ([S0.4](03-fofoca-blobs/findings/s04-multi-source-throughput.md)). That work
  also re-derived the per-connection ceiling claim this document flagged as
  unbacked, and found the *mechanism* RFC 01 named to be wrong.

Neither displaces Phase 1. There is still no baseline for the thing users
experience, and the caveat below about `agent-share bench` measuring the wrong
shape applies to those numbers too.

Scope: the whole byte path, native and browser — mount I/O, the WebRTC/QUIC
lane, the JS↔wasm boundary, the gossip runtime, and SIMD. Started as a question
about SIMD specifically; SIMD turned out to be the smallest item in it and is
kept here as one section rather than its own document.

Self-contained: findings, expected magnitudes, phases, edge cases, verification.

## Context: what is broken and why

**We do not know how fast `agent-share` is, and we have never known.**

`docs/research/iroh-webrtc/` was deleted as misleading. It should not be cited
and should not be restored from git history as evidence — its numbers are the
reason this document opens this way. Note that
[`01-every-peer-a-seeder.md`](01-every-peer-a-seeder.md) leans on those numbers
for its throughput argument (the per-connection SCTP ceiling claim in its
Context and Edge-cases sections); that argument now has no backing and should
be re-derived, not inherited.

> **RE-DERIVED, and the mechanism was wrong.** The conclusion survives —
> **[measured]** a second source is worth ≥1.61×
> ([S0.4](03-fofoca-blobs/findings/s04-multi-source-throughput.md)) — but the
> *cause* RFC 01 named does not apply: **[verified]** the data channel is
> negotiated **unreliable and unordered**
> (`Reliability::MaxRetransmits { retransmits: 0 }`,
> `fofoca-iroh-webrtc-transport/src/host/jsep.rs:104-106`), so the
> reliable-ordered SCTP receive-window stall cannot be what limits it. Any
> figure derived from 128 KiB/RTT should be dropped. RFC 01 now carries the
> correction inline, and its two dangling citations to this directory are
> marked.

This document's own warning applies to that re-derivation: the numbers come from
`agent-share bench`, which measures a serial single-stream path (see below).


What we have instead is `agent-share bench`, and it measures less than it looks
like it does. `fill_once` is awaited one at a time and opens its own bi-stream
each time (`crates/agent-share/src/mount/bench.rs:493`), so its output is a
**single-stream serial** number. A real mount does concurrent reads. The bench
can report a healthy figure while actual transfers are capped by something it
structurally cannot see.

So the situation is: no baseline, a benchmark that models the wrong thing, and
a code path with at least six independent mechanisms that could each plausibly
be the limiter. Consequences:

1. **Any optimization landed today is a guess.** Including the ones in this
   document.
2. **The obvious suspects are mutually confounding.** Depth-1 requests, silent
   packet drops, and four blocking-pool round trips per read would each, alone,
   present as "the network is slow".
3. **The one number we do have is not comparable to the thing users experience.**
   Nobody has recorded a `serve` → mount → `cp` of a large file.

This RFC exists to make the list falsifiable, not to pick winners. Phase 1 is
measurement, and the phase order is deliberate: **no perf change lands before
a baseline exists.**

Findings marked **[verified]** were confirmed by reading the code directly.
Unmarked findings come from a survey pass and must be re-checked before anyone
acts on them.

## Load-bearing facts about how bytes move today

- One QUIC bi-stream per request, and the 32-byte bearer secret is re-sent on
  every one (`mount/consume.rs:431-451`). Ops are `OP_MANIFEST` / `OP_WATCH` /
  `OP_READ` / `OP_BENCH`; `OP_READ` carries exactly one `(index, offset, len)`
  triple. There is no multi-range op and no batching.
- NFS negotiates `rsize=131072`; `MAX_READ_LEN` is 256 KiB
  (`crates/agent-share-proto/src/framing.rs:80`), so the ceiling is never
  reached because the client never asks for more than `rsize`.
- Transport config is left at iroh's defaults deliberately
  (`crates/agent-share/src/lookup/mod.rs:114-116`), which means
  `max_concurrent_bidi_streams: 100` and `send_fairness: true`. The latter
  round-robins concurrent responses, so N concurrent reads all complete near
  the tail rather than progressively. The same defaults set quinn's
  `stream_receive_window` to **1.25 MB** (12.5 MB/s × an assumed 100 ms RTT)
  and `send_window` to 8× that; the connection-level `receive_window` is
  unbounded. Per-stream throughput is therefore capped at 1.25 MB / RTT —
  invisible today because a 256 KiB read fits inside the window, binding the
  moment read depth rises.
- The browser peer terminates a **real iroh QUIC connection inside wasm**
  (`crates/agent-share-wasm-client/Cargo.toml` takes `iroh` with `tls-ring` +
  `unstable-custom-transports`) carried over a WebRTC data channel. So file
  bytes are encrypted twice: once by `ring` compiled to wasm, once by the
  browser's native DTLS/SRTP. The data channel is deliberately unordered and
  unreliable (`ordered: false`, `max_retransmits: 0`,
  `crates/fofoca-iroh-webrtc-transport/src/web/jsep.rs:235-239`) — do not
  "fix" backpressure by making it reliable; that would stack ARQ and
  head-of-line blocking underneath QUIC's own.
- The mesh shares the *same* iroh endpoint as the data path
  (`mount/produce.rs:124-127`, `mount/consume.rs:194-197`). Gossip and file
  bytes share one UDP socket and one congestion domain.
- Metadata is cached well and deliberately: all attribute answers come from the
  in-memory tree (`mount/nfs.rs:336-338`), and `actimeo=10` is a documented
  choice. **File data is not cached at all.**

## Tier 1 — the six that most likely dominate

### 1. Reads are strictly serial, depth 1 [verified]

`web/src/download.ts:45-64`, `web/src/mount.ts:161-170`

`ReadableStream.pull` awaits one `reader.read()` before issuing the next.
Throughput is therefore hard-capped at `256 KiB / RTT` — roughly 5 MB/s at
50 ms, 25 MB/s at 10 ms — regardless of available bandwidth. Each read is an
independent `open_bi()`
(`crates/agent-share-wasm-client/src/lib.rs:458-461`), so they are fully
independent and nothing in the protocol prevents pipelining.

The native side has the same shape: only the kernel NFS client's readahead
window produces any overlap, and **`readahead=` is not set in either mount
option string** [verified] (`mount/consume.rs:590,597`).

- **Magnitude:** potentially the whole ballgame on any non-loopback link. A
  1 GiB transfer is 8192 strictly sequential round trips.
- **Effort:** low. A bounded in-flight window in `download.ts`/`mount.ts`; one
  line to test `readahead=` natively.
- **Killed by:** raising depth to 4–8 and seeing throughput not move.

The structural alternative to N parallel streams is **one persistent bi-stream
with pipelined, request-tagged reads**: it also retires the per-request secret
resend, the per-request stream setup, and the 2 s lingering close that
inflates the live stream count against the 100-stream default. Choose between
them on phase 1–2 data. Whichever shape wins, quinn's default
`stream_receive_window` becomes the next cap the moment depth rises — a
depth-8 run plateauing near `1.25 MB × N / RTT` is the window, not depth. The
two are confounded and must be varied together in phase 2.

### 2. The data channel drops instead of applying backpressure [verified]

`crates/fofoca-iroh-webrtc-transport/src/web/transport.rs:604-627`, `:251-266`

`poll_send` takes `_cx`, never registers the waker, and unconditionally returns
`Poll::Ready(Ok(()))`. QUIC therefore gets **no pacing signal at all** and
keeps handing down transmits regardless of downstream state. Then there are two
silent drop points: `try_send` on a full 256-slot queue (`:625`), and the pump
discarding whenever `bufferedAmount` exceeds a 1 MiB cap (`:256-266`). Every
drop is a QUIC packet loss → retransmit → congestion-window collapse.

`bufferedAmountLowThreshold` and `onbufferedamountlow` do not appear anywhere in
the repo. The inbound path has the mirrored problem: `try_send` at `:194` also
drops on a full queue, while the *second* hop was already changed to
`send().await`.

- **Magnitude:** large and non-linear. Sustained loss on a fast local channel is
  how a link that should do 100 MB/s ends up in single digits.
- **Effort:** medium. Register the waker; replace both drops with awaits driven
  by `onbufferedamountlow`.
- **Killed by:** the drop counter at `:258` staying at zero under sustained
  load. The warn is already there and already rate-limited.

### 3. The producer reopens the file on every read [verified]

`crates/agent-share/src/mount/produce.rs:361-370`

`File::open` → `metadata()` → `seek()` → `read()` → drop-`close()`. Five
syscalls and **four sequential `spawn_blocking` round trips per 128 KiB** —
about 8192 of each per GiB — and the `close(2)` on drop runs synchronously on a
tokio worker. There is no fd cache anywhere in `mount/`.

The `fstat` is load-bearing: `produce.rs:353-360` explains it bounds the read
against the live length for appended files. But it can run on a cached fd, and
`pread` would remove the `seek` entirely and make the fd shareable across
concurrent readers.

- **Magnitude:** four blocking-pool handoffs serialize into every request's
  latency. Interacts with #1 — at depth 1 they are pure added latency.
- **Effort:** medium. An LRU of open files keyed by manifest index, plus `pread`.
- **Killed by:** a syscall count showing the reopen is already cheap relative
  to the round trip, i.e. the transfer is RTT-bound not producer-bound.

### 4. All metadata crosses JS↔wasm as a JSON string

`crates/agent-share-wasm-client/src/lib.rs:1560-1563`

```rust
let json = serde_json::to_string(value)?;
js_sys::JSON::parse(&json)
```

`serde-wasm-bindgen` is not a dependency. Every structured value goes Rust
struct → JSON `String` in wasm memory → UTF-8→UTF-16 transcode → `JSON.parse`.
This carries the **entire manifest** (`lib.rs:414`), and again on every watch
push (`lib.rs:1226`) — full manifest by design, not a delta. A 10k-file share is
roughly 1.5–2 MB of JSON parsed on the main thread per push.

File bytes correctly bypass this (`lib.rs:457` returns raw `Vec<u8>`). Only
metadata is affected — but metadata is the whole tree.

- **Magnitude:** tens of ms of main-thread jank per watch push, scaling with
  tree size. Does not affect steady-state byte throughput.
- **Effort:** low. `serde-wasm-bindgen` is a drop-in for `serde_wasm`.
- **Killed by:** profiling showing watch pushes are rare enough not to matter.

### 5. The CRDT hydrates its whole document 2–4× per change [verified]

`crates/agent-habilis-mesh/src/doc/mod.rs:325-330`

```rust
let before = self.to_json();
self.apply(change, hash, frame.clone());
self.drain_pending();
let after  = self.to_json();
Ingested::Applied { changed: before != after, doc: after }
```

Two full document hydrations plus a deep recursive `Value` comparison, purely
to compute a `changed: bool`. On the gated `meta` channel, `forges_foreign_entry`
(`:182-186`) runs first and adds a full `fork()` plus two more hydrations.
**Every peer pays this for every other peer's card publish.**

Changes themselves are correctly gossiped incrementally — the doc is never
`save()`d. The cost is entirely in the JSON hydration used for change detection.

- **Magnitude:** scales with document size × mesh chatter. Invisible on a
  two-peer mesh, quadratic-feeling on a busy one.
- **Effort:** medium. Compare heads, or use automerge's own diff, instead of
  hydrating twice.
- **Killed by:** documents staying small enough that `to_json` is trivial.

### 6. The 7.0 MB wasm is never optimized or compressed [verified]

`tasks/src/web_wasm.rs:44-50`, `web/build.ts`

`dist/web/agent_share_wasm_client_bg.wasm` is 7,378,036 bytes, of which about
1.74 MB is the `name` custom section — debug symbols the browser never reads.
Bare `wasm-bindgen` does **not** run `wasm-opt` (unlike `wasm-pack`), and
neither `web/dev.ts` nor `web/preview.ts` sets `Content-Encoding`. There is no
precompressed artifact.

The crate's `[profile.release]` sets only `opt-level = "s"` and `lto = true` —
no `strip`, no `codegen-units = 1`.

Separately, the intended lazy load is defeated: `web/src/wasm.ts:32-46` uses a
dynamic `import()`, but the built output contains none. Bun inlined ~70 KB of
glue into *both* entry chunks with no `splitting: true` in `web/build.ts:11-16`.

- **Magnitude:** time-to-interactive, not throughput. `wasm-opt --strip-debug`
  plus brotli should land ~7 MB → ~1.5–2 MB.
- **Effort:** low. One build step and one `Content-Encoding` header.
- **Killed by:** nothing — this one is close to free. It is Tier 1 on effort,
  not on magnitude.

**These interact.** #1, #2 and #3 together are a depth-1 request pattern, over a
link that silently drops packets, against a producer paying four blocking-pool
round trips per request. Measuring one while the others are live will produce a
misleading answer. They have to be characterized together, and **none of them is
a compute problem.**

## Tier 2, by area

### Mount I/O, native

- **~4 full-payload memcpys and 2 memsets per 128 KiB delivered.** Producer
  allocates a zeroed 128 KiB `Vec` (`produce.rs:371`); `tokio::fs::File` does
  not read into it but allocates its own buffer and memcpys; `write_all` copies
  into the QUIC send buffer; the consumer allocates and zeroes again
  (`consume.rs:496`); `nfsserve` copies a third time while growing an empty
  `Vec` through ~17 doubling reallocations. The structural blocker is
  `ByteSource::read -> Result<Vec<u8>>` (`nfs.rs:22`) — an owned `Vec` makes
  pooling impossible, and the vendored `nfsserve` VFS trait forces the same
  shape above it.
- **The connection mutex is held across the entire redial loop** [verified]
  (`consume.rs:377-426`). The guard is taken at the top of `connection()` and
  held through a retry loop containing a 3 s sleep, bounded by a **90 s**
  discovery deadline, and possibly a full ICE negotiation. This is a severity
  bug, not a tuning item: one connection blip stalls *every* concurrent read
  behind one mutex for up to a minute and a half. **Fix this regardless of what
  the benchmarks say.**
- **A one-byte edit re-encodes the whole manifest under the write lock**
  (`live.rs:245-250`): deep-clones every `DirEntry` and `FileEntry`, then
  serializes the entire tree. On a 70k-file share that is ~70k `String`
  allocations per debounced batch — and it runs holding the `RwLock` that
  `path_of` needs, so every in-flight read on the producer blocks for its
  duration. The consumer mirrors it: `build_tree` runs on every watch frame
  (`consume.rs:288`), cloning every directory's children vector to re-sort
  (`nfs.rs:315-334`).
- **`scan()` is called blocking inside async `serve`** (`produce.rs:49`) with no
  `spawn_blocking`, while `live.rs:301-303` deliberately wraps the identical
  call with a comment explaining exactly why. Startup-only, but unambiguous.
- **Scan micro-costs**: `sort_by_key` invokes its key function per comparison
  and `DirEntry::file_name()` allocates (`scan.rs:31`) — `sort_by_cached_key`
  makes that n instead of n·log n; and `child.path()` is called three times per
  entry (`scan.rs:39,55,63`).
- **The lingering stream close**: `produce.rs:290-291` waits up to 2 s on
  `send.stopped()` after `finish()`. This is a deliberate correctness guard —
  the comment explains it stops a fast/loopback connection racing stream
  teardown ahead of the last bytes — so it is not waste. But it does hold each
  stream and its task alive for an extra RTT, and with one stream per request
  that inflates the live stream count against the 100-stream default. Worth
  measuring under concurrency before assuming it is free; **do not remove it
  without understanding the race it prevents.**

### Browser and the JS↔wasm boundary

- **Copy counts**: 5 per 256 KiB producer-side read; 4–5 per inbound datagram;
  plus roughly 218 `Vec` allocations per read on the outbound path
  (`crates/fofoca-iroh-webrtc-transport/src/web/transport.rs:625`, one
  `to_vec()` per ~1200 B QUIC datagram). One copy is
  provably redundant — `web/src/mount.ts:167` re-copies a buffer the
  generated glue already `.slice()`d into fresh detached memory, with a comment
  justifying it on a premise that does not hold for this glue.
- **`getFile()` on every read**
  (`crates/agent-share-wasm-client/src/produce.rs:864-882`)
  — no `File`/`Blob` cache; `LiveState` stores only the
  `FileSystemFileHandle`. A 1 GB download at 256 KiB granularity is ~4096
  FS-Access round trips, each re-stat'ing the file.
- **The JS rescan re-walks the whole directory every 2 s**
  (`web/src/produce.ts:41,60-78`), calling `getFile()` per file for `.size` and
  `.lastModified`, with no mtime short-circuit, no incremental scan, and no
  guard against a scan overrunning its own interval.
- **`snapshot()` deep-clones the tree on every `update()`**
  (`crates/agent-share-wasm-client/src/produce.rs:181`) including the
  overwhelmingly common no-change case; it is only needed for the oversize
  rollback.
- **`Reflect::get` in the listing loop** (`produce.rs:599-661`): four
  `Reflect::get` calls per file, each constructing a fresh `JsValue` key, on
  every 2 s rescan.
- **Front end is `visage-dom`, not React** — the equivalents of re-render
  problems are present and arguably worse for lack of a memoization boundary:
  `buildTree` is O(N²) on wide directories (`tree.ts:71-86`, a linear `find`
  per path component); `localeCompare` without a hoisted `Intl.Collator`
  (`tree.ts:135`); a full tree flatten and reduce *inside the render closure* on
  every progress tick (`App.tsx:684-685` — ~4096 times per GB); and
  `MiddleTruncate` rendered with no `budget`, taking the measuring branch that
  forces two synchronous layouts and constructs one `ResizeObserver` **per row**
  (`ColumnView.tsx:366,411`), with no virtualization on the list.
- **`getStats` is called up to 3× per peer per second**, each walking the full
  report and allocating a `String` per entry (`lib.rs:275-322` →
  `crates/fofoca-iroh-webrtc-transport/src/web/transport.rs:406-459`), driven
  by a 1 Hz interval in `TechInfo.tsx`.
- **Connect latency is polled, not evented**: `jsep.rs:30` `POLL_MS = 50`, with
  `setTimeout` loops in `wait_ice_complete` and `wait_channel_open` instead of
  `onicegatheringstatechange`/`onopen`. `settled_path_label` can add up to 3 s
  to time-to-first-byte, and `connect_webrtc` awaits it before resolving.
- **No Web Worker anywhere.** The full QUIC/TLS stack, the entire gossip node,
  manifest encode/decode, JSON marshalling, and the directory rescan all share
  the main thread with layout and paint.

### Gossip runtime

- **`MeshDoc` state is unbounded** (`doc/mod.rs:92-98`) [verified]: `frames`
  retains the complete signed `Message` for every change ever applied, for
  process lifetime, across two documents. `pending` has no TTL and no cap at
  all — a peer flooding changes whose deps never arrive grows it without bound.
  `tick_prune` only sweeps `reassembly`.
- **An idle mesh is not quiet**: the anti-entropy arm emits three signed
  messages every 10 s unconditionally (`event_loop.rs:348-361`) with no
  "heads unchanged since last round" guard — 18 signed frames per minute per
  node, forever, each parsed and Ed25519-verified by every peer.
- **Dedup happens after verify** (`gossip/recv.rs`), so every duplicate delivery
  pays a full `canonical_bytes` + Ed25519 verify. The ordering is deliberate and
  security-correct — dedup-first lets a forged id poison the window — but a
  pre-gate on a hash of the *raw wire bytes* would skip byte-identical repeats
  without weakening that, since an identical frame cannot be a forgery attempt.
- **`canonical_bytes` costs more than its known call sites suggest**: the
  always-on message log (`logging/messages.rs:37-41`) recomputes it per frame,
  so a broadcast pays it twice on send and twice on receive. Threading the
  already-computed value into `log_in`/`log_out` halves it.
- **O(N²) shard reassembly via the log fallback** (`daemon/state.rs:641-658`):
  every non-final shard returns `Buffered` and falls through to a full
  1000-entry log scan. Also `synthesize_logical` concatenates with
  `String::new()` and no `with_capacity` despite tracking the byte total.
- **A 400 ms timer that is a no-op ~99.9% of ticks** (`event_loop.rs:669`,
  `util/tuning.rs:349`): the reclaim arm re-polls the whole `select!` 150×/min
  to evaluate two `Option` compares, and is only useful inside a 6 s window
  armed on `NeighborDown`. The ping arm at `:573` already demonstrates the
  conditional `sleep_until_opt` pattern.
- **Blocking file I/O on the reactor thread** (`daemon/state_file.rs:136-192`)
  — called from async with no `spawn_blocking`, on every roster change.
- **Double serialization on send**: `gossip/broadcast.rs:58` calls `wire_len()`,
  which serializes and throws the bytes away, purely to size-check — then
  `serialize()` does it again, and already has its own size check.
- Locks are otherwise clean: no sync guard is held across an `await` anywhere in
  the crate. The one coarse lock is `Arc<Mutex<BlobStore>>`
  (`blob/produce.rs:38`), which caps blob-serve concurrency at 1.

### SIMD — the original question, and the smallest item here

**Native: there is nothing to do.** Every bulk-byte path already runs vectorized
code inside a dependency with runtime dispatch — `ring`'s assembly for QUIC
record crypto, `blake3`'s SSE2/AVX2/AVX-512/NEON. First-party code contains no
`std::arch`, no `target_feature`, and no bulk loops worth vectorizing; `Sha256`
only ever touches tickets and identity strings. `-C target-cpu` is not available
for releases, which cross-compile through the vendored zig toolchain
(`tasks/src/build.rs`) and must run on generic hardware.

The genuine native win in this area is algorithmic, not vector: **`bs58` is an
O(n²) big-integer base conversion sitting on bulk paths** — every automerge
change is base58-encoded into a JSON body (`doc/wire.rs:64,76`) and anti-entropy
packs digests through it on a timer (`gossip/antientropy.rs:39-59`). No amount
of SIMD fixes a quadratic loop.

**Wasm: real, but purely configuration.** The client builds at
`opt-level = "s"` with no `+simd128`, so LLVM emits no vector instructions and
every SIMD-capable crate in the graph silently takes its scalar fallback.
`memchr`, `simdutf8` and `blake3` all gate on
`#[cfg(target_feature = "simd128")]` — wasm has no runtime feature detection, so
this is decided entirely at compile time. Levers in order:

1. **`opt-level = "s"` → `3`** in `crates/agent-share-wasm-client/Cargo.toml`.
   Size-optimization suppresses inlining; simdutf8's own wasm guide calls this
   out. Nothing else on this list works through it.
2. **`-C target-feature=+simd128`** in **both** `.cargo/config.toml` files — the
   root and the wasm client's. They are separate workspaces and the flag is not
   inherited. Free given a latest-Chrome/latest-Safari support target: simd128
   shipped in Chrome 91 and Safari 16.4 (March 2023). The usual caveat — a
   `+simd128` module fails *validation* outright on an engine without it, rather
   than degrading — does not apply to us. No feature detection, no dual builds.
3. **`blake3` feature `wasm32_simd`** — ~~upstream reports 6× on large inputs
   under Wasmtime plus a later ~20%~~. Requires adding a direct dependency on
   the wasm client purely to enable it through feature unification.
   ~~Rated modest: blake3 is iroh's hashing and is not obviously on our per-byte
   path.~~

   > **MEASURED, and both halves of the rating were wrong.**
   > [`03-fofoca-blobs`](03-fofoca-blobs/findings/s03-hash-throughput.md)
   > benchmarked it: **1.80× under node/V8**, not 6×. The 6× figure is
   > Wasmtime's; engine choice evidently matters enormously here, so quote it
   > as engine-specific or not at all. Absolute numbers: 1197 MiB/s portable →
   > 2160 MiB/s with `simd128`, which is **92% of native single-threaded
   > blake3**. Costs +12 KB of module.
   >
   > The "not obviously on our per-byte path" rating also flips **if
   > [RFC 03](03-fofoca-blobs/README.md) lands**: bao verification puts blake3
   > on *every non-origin byte*, and outboard construction becomes a latency the
   > user waits on before a file's first swarm fetch. Under that design this is
   > the largest wasm lever in this section, not a modest one.
   >
   > Caveat in the other direction: at ~2 GiB/s, hashing is far above the link
   > ceiling either way. This buys **latency, not throughput** — see that
   > document's Performance section, which is emphatic about the distinction.
4. **`--cfg curve25519_dalek_bits="64"`** — wasm32 is a 32-bit target so the
   crate picks 32-bit limbs despite wasm having native `i64`. Handshake cost,
   not throughput; it moves time-to-first-byte, not MiB/s.

Two hypotheses that could not be sourced and must be measured rather than
assumed:

- Whether clang autovectorizes `ring`'s wasm C core under
  `CFLAGS_wasm32_unknown_unknown="-msimd128"`. A `RUSTFLAGS` target-feature does
  **not** reach C compilation, and ring's C is what `tasks/src/web_wasm.rs`
  needs a wasm-capable clang for. If it does help, it is the largest wasm win
  available, because QUIC record crypto is directly on the per-byte path. If it
  does not, the fallback is steering rustls' cipher-suite preference toward
  ChaCha20-Poly1305 for the wasm build — without hardware AES, ChaCha wins in
  software, and rustls' default ordering was tuned for machines that have AES-NI.
- The commonly-cited 20–40% for the curve25519 word-size override. Upstream
  documents only that overriding "may be required for better performance".

## Phases

**Phase 1 — baseline. Nothing else starts until this exists.**

Run the matrix with the benches that already exist: native `agent-share bench`,
browser `ShareClient::bench()`
(`crates/agent-share-wasm-client/src/lib.rs:490`) driven from
`web/src/lab.ts:102` on `/lab`. Cells: native↔native and native↔browser;
Chrome and Safari; both transports; **both directions** (browser-as-producer
exercises the `read_from_handle` double copy, browser-as-consumer does not); and
**at least two RTTs** — `dnctl` + `pfctl` inject delay on macOS with no extra
tooling. A loopback number answers "how fast can the CPU go"; a 50 ms number
answers "does that matter". The gap between them *is* the CPU-bound question.

Record a real `serve` → mount → `cp` of a large file alongside the bench number.
**If they disagree, say so; do not quote the flattering one.**

Every row states machine, OS, browser version, transport, direction and RTT.
Missing provenance is what made the previous numbers untrustworthy. Write to a
new file under `docs/` — do not resurrect the deleted tree or its table shape.

**Phase 2 — diagnostics that discriminate between the Tier 1 items.** Still
shipping nothing; all patches reverted afterward, all numbers written down.

- Read the drop counter at
  `crates/fofoca-iroh-webrtc-transport/src/web/transport.rs:258` under
  sustained load. The warn is already in place and rate-limited. Nonzero
  confirms #2's mechanism.
- Patch read depth to 4–8 in `download.ts` and re-run one cell. Roughly linear
  scaling confirms #1 and reorders everything below it.
- Patch quinn's `stream_receive_window` up (e.g. 8 MB) via `TransportConfig`
  in `crates/agent-share/src/lookup/mod.rs` and re-run the depth-patched cell.
  If throughput moves, the 1.25 MB flow-control default — not depth — was the
  binding constraint. iroh's docs warn against transport tuning, so this stays
  a diagnostic, not a fix.
- Add `readahead=` to the mount options (`consume.rs:590,597`) and re-run the
  `cp`. Same hypothesis, native side, one line.
- Raise `rsize` from 131072 to the `MAX_READ_LEN` of 262144 in the same mount
  options and re-run the `cp`. Halves the round-trip count at depth 1,
  independent of `readahead=`; one more line, same cell.
- Count `openat`/`lseek`/`close` on the producer during one mount `cp`. Roughly
  4 syscalls per 128 KiB turns #3 from a plausible idea into a sized one.

**Phase 3 — microbenchmark harness, scoped to what phases 1–2 justify.** If the
limiter turns out to be request depth or packet loss, a SIMD-oriented harness is
premature; build only the manifest and body-scan benches and stop. Design
constraints are recorded in "Edge cases and risks" below.

**Phase 4 — fixes, in measured order.** Not specified here on purpose. The
ordering is an output of phases 1–3, not an input.

The one exception to the measure-first rule: **the connection-mutex hold across
the redial loop** (`consume.rs:377-426`) is a correctness/availability bug that
happens to live in this document. It does not need a benchmark to justify
scoping a lock.

## Edge cases and risks

1. **`agent-share bench` measures the wrong shape.** Single-stream serial, one
   bi-stream per fill. Treat it as a latency probe and a floor, never as mount
   throughput. This is the trap the deleted research fell into.
2. **The mesh shares the data path's endpoint and congestion domain**
   (`produce.rs:124-127`). A "throughput" measurement on a busy mesh is
   measuring two things. Note mesh state in every row.
3. **`divan` cannot run on wasm.** Verified against vendored source:
   `divan-0.1.21/src/thread_pool.rs` uses `std::thread::Builder`,
   `std::thread::park` and `std::process::abort`, and `util/mod.rs:102` calls
   `available_parallelism`; its CLI has no JSON output. Keep divan as the native
   interactive leg — its `AllocProfiler` is what proves an allocation actually
   went away, which no wall-clock number can. The portable leg needs a separate
   crate whose only dependency is `web-time`.
4. **`Instant::now` compiles on `wasm32-unknown-unknown` and then panics.** This
   is the failure mode `crates/agent-habilis-mesh/tests/wasm_runtime.rs` exists
   to guard, and the reason `.cargo/config.toml` pins a
   `wasm-bindgen-test-runner`. Every wasm breakage this repo has had compiled
   cleanly first.
5. **`performance.now()` is clamped** — 1 ms in Firefox and Safari, 100 µs in
   Chrome without cross-origin isolation. Never time a single iteration on wasm:
   batch to a floor, divide, take a median, and emit an explicit
   `clock_resolution_exceeded` flag rather than fabricating a number.
6. **`RUSTFLAGS` replaces `target.wasm32-unknown-unknown.rustflags`; it does not
   append.** Setting `+simd128` that way silently drops
   `--cfg getrandom_backend="wasm_js"` and the wasm build fails with a message
   pointing nowhere near the cause. Three `getrandom` generations are in this
   graph. Any flag experiment must restate the base.
7. **Nothing in this repo has ever *linked* a wasm binary for
   `agent-share-proto`** — CI only `cargo check`s it, and `check` does not link.
   Stand up an empty wasm test target first. Encouraging sign: `Cargo.lock`
   shows `iroh-base` pulls `curve25519-dalek`/`ed25519-dalek` but **not** `ring`,
   so it should not need the wasm-capable clang the mesh leg does.
8. **Every bench file is linted** by the existing
   `cargo clippy --workspace --all-targets -- -D warnings` at `pedantic`, and
   `allow_attributes` is `warn` — suppressions must be `#[expect(…, reason)]`,
   never `#[allow]`.
9. **`unsafe_code = "deny"` workspace-wide, on stable 1.95.** Hand-written
   `core::arch` intrinsics need a per-crate opt-out and `core::simd` needs
   nightly. Everything proposed in the SIMD section is a safe-API crate or a
   build flag; **keep it that way.**
10. **Turning on `+simd128` and `opt-level = 3` grows the `.wasm`.** Speed is the
    stated priority, but the size delta must be reported alongside throughput,
    not discovered later. It compounds with finding #6.

## Rejected alternatives

- **Hand-written SIMD intrinsics in first-party code.** There is nothing to
  vectorize: the wire codec is a 16-byte header parse, hashing touches tickets,
  and there is no compression in the graph. It would also require opting out of
  `unsafe_code = "deny"` for no measured gain.
- **`-C target-cpu=x86-64-v3` for releases.** Blocked by cross-compilation
  through vendored zig, and `ring`/`blake3` runtime-dispatch regardless of the
  baseline — so it would only affect our own scalar code. Real compatibility
  cost, negligible benefit.
- **Swapping `ring` for `aws-lc-rs` on native** (iroh exposes `tls-aws-lc-rs`).
  AWS-LC has VAES/AVX-512 AES-GCM paths that ring's older BoringSSL-derived
  assembly lacks, and kernel-side measurements of the same VAES work show large
  gains. Deferred, not dismissed: it only matters if we are CPU-bound on crypto,
  which phase 1 will tell us, and enabling both providers panics at runtime
  while `str0m-aws-lc-rs` is already in the graph.
- **Restoring `docs/research/` from git history.** It was deleted for cause.

## Verification

1. A baseline exists at all: a new file under `docs/` carrying the full matrix,
   every row stating machine, OS, browser version, transport, direction and RTT.
2. The bench-vs-`cp` gap is recorded side by side, with the disagreement stated
   plainly if there is one.
3. All four phase-2 diagnostics are recorded, and all their patches reverted.
4. `cargo task ci` passes, including the wasm32 checks at
   `tasks/src/ci.rs:76-97`.
5. The harness can detect a change at all: flip `opt-level = "s"` → `3` with
   `+simd128`, re-run, confirm the wasm numbers move, revert. **If it cannot see
   that, it cannot see anything**, and it needs fixing before any claim in this
   document is worth testing. Run the same canary natively with
   `stream_receive_window`: raise it, re-run the depth-patched cell, confirm
   the number moves, revert. It is the knob most likely to silently eat a
   depth fix's gains.
6. Microbenchmark numbers reproducible within noise across three runs; if not,
   the synthetic input is too small.
7. Findings not marked **[verified]** were re-checked against the code before
   any of them were acted on, and unsourced claims are still marked unsourced.
