# RFC 01: every peer a seeder

Status: **draft** — design agreed with the user, no work started.
Scope decided with the user: all four goals (resilience, throughput, fan-out,
producer offload); swarm the whole-file paths first and the NFS mount last;
verification via lazily-computed bao ranges.

Self-contained: findings, design decisions, phases, edge cases, verification.

## Context: what is broken and why

A share is a **star**. The ticket names one producer and every consumer reads
every byte from it: `crates/agent-share/src/mount/consume.rs` holds one
`RemoteClient` → one `Connection` → the origin, redialled on drop. Consequences:

1. **A share dies with its producer.** Close the tab or the terminal and every
   mount goes dark, even when another peer on the mesh holds a complete copy.
2. **Throughput is capped per connection, not per link.** The WebRTC data
   channel ceiling is SCTP's 128 KiB receive window — ~18 MB/s at 0 ms RTT
   collapsing to <2 MB/s at 50 ms (`docs/research/iroh-webrtc/README.md`). One
   source cannot be tuned around it; a second source is worth ~2×.
3. **Fan-out is linear on the producer's uplink.** Ten consumers of one large
   file are ten full copies out of one pipe.
4. **Peers that already hold the bytes are idle.** A browser that mirrored a
   share holds a complete, readable, enumerable copy and is never asked to serve
   it (`web/src/mount.ts:115`, `web/src/App.tsx:499`).

The original framing — "an iroh-blob per file, ask on the gossip, seed once
complete" — is directionally right, but three of its four moves are already
built or are the wrong shape here. See *Rejected alternatives*.

## Load-bearing facts about the existing wire and peers

- **The mount protocol is already symmetric, by accident of the security
  model.** A producer authenticates a read with one line —
  `if &header[..SECRET_LEN] != secret` (`mount/produce.rs:247`) — and that
  secret is the ticket secret, which every mesh member holds *by definition*:
  the mesh id is `SHA256("agent-share/mesh/v1" ‖ secret)`
  (`agent-share-proto/src/mesh_key.rs:40`). Any peer can authenticate any other
  peer's READ with **zero new code**.
- **The consumer read path is already behind a trait.** `ByteSource`
  (`mount/nfs.rs:19`) is `async fn read(&self, index, offset, len)`, and
  `RemoteFs<S: ByteSource>` is generic over it. Multi-source is a new impl, not
  a rewrite — `nfs.rs` does not change.
- **The availability index already exists and is replicated.** Every ticket
  holder is a gossip peer (`mount/mesh.rs`, mesh derived from the ticket
  secret), and each publishes a `PeerCard` to `/peers/<nick>/card` on the
  automerge `Channel::Meta` under a `SelfWriteGate`
  (`agent-share-proto/src/client.rs:34`). No request/response protocol is
  needed.
- **The native consumer never joins the share mesh.** `consume.rs` contains
  zero mesh references; only `produce.rs:118` calls `mesh::join`. The *browser*
  consumer does (`crates/agent-share-wasm-client/src/lib.rs:244`, role
  `"consumer"`). So on the CLI side there is currently no roster to select
  sources from.
- **`ShareDriver::on_app_frame` returns `false`** in both `mount/mesh.rs` and
  `wasm-client/src/mesh.rs` — the app-frame plane on the share mesh is entirely
  unused.
- **Index stability is the READ-address invariant.** `LiveTree`
  (`mount/live.rs:1-19`) is append-only with tombstones; indices are a
  *per-serve* invariant, deliberately **not** re-derivable from sorted
  `rel_path` (`scan.rs:31` sorts per-directory, but the DFS `stack.pop()` walk
  means the global order is not sorted either).
- **`answer_read` deliberately does not clamp to the manifest size**
  (`produce.rs:326`, rationale at `:336-339`: a growing file must read past its
  scanned length). A short read therefore means EOF today.
- **There is no browser persistence at all.** Zero OPFS, zero IndexedDB in
  `web/src` or `crates/agent-share-wasm-client/src`.
- **The browser can hash.** It already reads bytes on demand
  (`wasm-client/src/produce.rs:865`, `blob.slice()` → `array_buffer()`),
  `blake3 1.8.5` is already in `Cargo.lock:551` (transitive), and `bao-tree`
  minus its `fs` feature is wasm-clean — iroh-blobs' own wasm CI builds exactly
  that feature set. The only browser-specific cost is having nowhere to cache
  the outboard.
- `MAX_DIRECT_PEERS = 16` (`agent-habilis-mesh/src/transport/webrtc.rs:451`),
  arbitrated by `SignalAdmission` and **shared with the mesh itself**. Each new
  peer costs a JSEP round (`JSEP_DEADLINE` 20 s).
- Gossip frames cap at `MAX_MESSAGE_SIZE = 3840`; `send_app` refuses an
  oversized signed frame and does not shard.

## The one thing that is actually missing

Four of the five pieces of a swarm exist: discovery, authentication, transport,
and the *serving* side. Exactly one is missing — **a peer that holds bytes** —
and no protocol design removes it.

| Consumer shape | Where bytes land | Can seed? |
|---|---|---|
| Native NFS mount (`consume.rs::attach`) | Kernel page cache only | **No.** Not enumerable, not addressable, not durable. |
| Browser download (`web/src/download.ts`) | A `FileSystemWritableFileStream` on a user-picked file | **No.** Write-only handle; cannot read back what it wrote, and the permission does not survive reload. |
| **Browser mirror (`web/src/mount.ts::syncMount`)** | A complete tree under a `FileSystemDirectoryHandle` taken with `{ mode: 'readwrite' }` (`mount.ts:115`), held for the tab's life (`App.tsx:499`) | **Yes, today, with no new storage.** |

That third row is why this is tractable: `web/src/produce.ts::scanDirectory`
already knows how to turn exactly that handle into the `{dirs, files}` listing
`ShareProducer.start` consumes. The browser mirror is a seeder that has simply
never been asked to serve.

## Design

**Keep `MOUNT_ALPN` and index addressing. Add lazily-computed bao outboards for
verification. Let peers that hold bytes serve the protocol they already speak.**

Three properties follow:

- **Nothing is hashed until someone wants to pull it from a third party.** The
  origin serves its own bytes unverified as today — QUIC/TLS to the ticket's
  endpoint id already authenticates that. An outboard is built only for a file
  a swarm fetch is about to touch, so `serve` stays an instant `stat` walk on
  both runtimes.
- **The ticket does not change and no issued ticket breaks.** The trust chain is
  already intact: a consumer learns hashes from the origin over a channel
  authenticated to the ticket's endpoint id, then accepts bytes from anyone,
  verified against those hashes. A hostile peer can only refuse or fail
  verification — bounded harm. A hostile *producer* can lie, but always could;
  it owns the bytes.
- **Partial-file seeding works**, because bao verifies *ranges*. A peer serves
  the chunk ranges it holds. This is what fan-out on one big file requires and
  what a whole-file-hash design cannot give.

Outboard overhead at 16 KiB chunk groups is ~0.4% of file size (4 MB per GB).

### Three guards, required in the first seeding commit

`RemoteFs` today has exactly one failure mode: *the producer is unreachable*. A
source set adds *peer on a stale tree*, *peer truncated*, *peer slower than the
origin*. Each is a silent-wrong-bytes class. These are not retrofittable.

1. **Tree generation.** Because `answer_read` does not clamp to the manifest
   size, a peer on an older manifest generation answers plausibly and wrongly.
   Every card carries a manifest fingerprint; a peer whose fingerprint differs
   from ours is not a candidate.
2. **A short read from a non-origin peer is a failure, never EOF.** Today
   `web/src/mount.ts:165` breaks on a zero-length chunk and then `truncate`s,
   and the NFS layer treats a short read as EOF. From a re-seeder holding a
   half-written mirror that is silent truncation. Origin short reads keep
   today's meaning.
3. **A seeder that isn't sure says "I don't have it."** A slot gets a handle
   only when the local file's `size` *and* `mtime` match the origin manifest's
   entry for that index; anything else → `None` → `ReadStatus::BadIndex`.

### Index authority

The origin stays the sole index authority. A re-seeder never assigns indices: it
serves the origin's manifest bytes **verbatim** for `OP_MANIFEST` and maintains
a local `Vec<Option<Handle>>` aligned to the origin's `files` vector by
`rel_path`. Tombstones map to `None`. `OP_WATCH` keeps working unchanged — the
origin publishes deltas, the re-seeder applies them to its copy and re-aligns.

Two ~20-line sibling constructors carry this plus the size/mtime gate:
`LiveTree::mirrored(root, origin_manifest)` in `mount/live.rs`, and its twin in
`wasm-client/src/live_state.rs`.

### Availability on the existing card

Two optional fields on `PeerCard` (`agent-share-proto/src/client.rs:34`), both
`skip_serializing_if`, so old peers stay parseable — `from_card_value` already
tolerates missing fields via `unwrap_or`. Purely additive.

- `tree: Option<String>` — 16 hex chars of `sha256(manifest_bytes)` over the
  exact bytes `OP_MANIFEST` returned. **The more important of the two**: it is
  what enforces guard #1. Needs no producer change (both ends already hold the
  bytes) and no new dependency (`mesh_key.rs` already hashes).
- `serving: Option<String>` — `"*"` for every live index, else sorted ASCII
  run-length ranges `"0-12,15,40-99"`. `"*"` is one byte and covers ~100% of
  whole-tree seeders; `syncMount` writes in manifest order, so a partial mirror
  is a prefix run — one range, ~6 bytes, even for a 10k-file tree. Hard cap
  ~512 bytes to stay inside `MAX_MESSAGE_SIZE = 3840` with CRDT framing. Over
  the cap: emit `"*"` if complete, else omit the field and simply not be
  advertised. Never split across frames.

Republish on a debounce — mirror `live.rs:39`'s 300 ms, or coarser (2 s plus one
on completion). **Never per file**: a card rewrite is a CRDT merge broadcast to
the whole mesh, and `syncMount` writes files one at a time.

Deliberately not in the CRDT: per-chunk availability. Automerge retains history,
so a high-churn bitfield would grow the meta doc without bound and every late
joiner would sync all of it.

### Source selection

Cap the peer set at **~4**, well under `MAX_DIRECT_PEERS = 16` — that budget is
shared with the mesh itself, and spending it on swarm sessions silently degrades
the mesh to relay. Rank by marginal cost, not capability:

1. Peers with a live session already — free.
2. Native peers (`PeerCard.transport == "unicast"`) — iroh hole-punching, no
   JSEP.
3. Browsers — a full JSEP round plus ICE gathering.

The origin is always a candidate and never counted against the ICE budget.
Least-outstanding dispatch (a slow peer sheds load naturally); three strikes
demotes, strikes reset when that peer's card changes; a **verification failure
is a permanent ban** for that share. Never open a new session for a fetch below
~16 MiB — ICE setup dominates. No hedging in v1: NFS readahead already supplies
aggregate parallelism, and `mount/bench.rs` exists to measure the tail before
paying for it.

## Phases

Each ships alone and is independently valuable.

**Phase 0 — the native consumer joins the mesh.** (~1 day)
Port the ~30-line block from `produce.rs:118-146` into `consume.rs::attach` with
`role: "consumer"`. Add `refresh_book()` / `known_cards()` and the
`on_meta_applied` / `on_peer_left` hooks to `ShareDriver` in `mount/mesh.rs` — a
near-verbatim port of `wasm-client/src/mesh.rs:200-217,429`.
*Ships alone:* CLI mounts get the peer counts the browser already shows.

**Phase 1 — manifest fingerprint.** (~half a day)
`MountManifest::fingerprint()` in `agent-share-proto/src/manifest.rs`; `tree` +
`CARD_TREE` on `PeerCard`.
*Ships alone:* `web/src/TechInfo.tsx` can say "3 peers on this tree, 1 on an
older one" — real diagnostic value today.

**Phase 2 — hoist the op-dispatch loop into `agent-share-proto`.**
The `OP_MANIFEST`/`OP_READ`/`OP_WATCH` match already exists twice near-verbatim
(`produce.rs:251` and `wasm-client/src/produce.rs`); phases 3–4 would add a
third and fourth. Hoist it over a small trait (`manifest_bytes()` / `read()` /
`subscribe()`). This is research-doc recommendation #9, already on the books,
and it is cheapest immediately **before** phase 3 rather than after.

**Phase 3 — a peer that has the bytes serves them.** *The core feature.*
Browser first: the mirror handle already exists and `produce.ts::scanDirectory`
already yields the right listing shape.
- `wasm-client/src/produce.rs`: `ShareProducer::reseed(listing, ticket,
  origin_manifest_bytes, card)` — identical to `start` except it mints no secret
  and no ticket, decodes the caller's, joins the *same* share mesh
  (`MeshPeer::join_share` already takes `&secret`), and serves the origin's
  manifest verbatim.
- Mirror constructors in `live_state.rs` and `mount/live.rs`, carrying the
  size+mtime gate.
- Native: a new `agent-share mirror <ticket> <dir>` verb — the CLI analogue of
  `syncMount`, whose output the *existing* `produce::serve` re-serves. Keep it a
  separate verb with different semantics; the lazy mount must stay diskless.
- Publish `serving` + `tree`, debounced.

**Phase 4 — bao outboards and verified ranges.**
- `agent-share-proto`: add `bao-tree` (`default-features = false`, no `fs`) and
  `blake3`; `OP_HASH = 5` returning root + outboard for one index, plus
  `MAX_OUTBOARD_BYTES`; extend the `wire_constants_are_pinned` golden test.
  **Do not touch `MountManifest::encode`** — an op an older producer doesn't
  know costs one stream (the `OP_WATCH`/`OP_BENCH` precedent), a changed
  manifest encoding breaks every issued ticket.
- The origin hashes one file on demand, caching the outboard beside the file
  (native) or in OPFS (browser — this would be the first OPFS use in the repo).
- Consumers verify every range from a non-origin source; a file with no hash yet
  falls back to the origin exactly as today.
- Files under one chunk group (16 KiB) never get a hash — the detour costs more
  than the bytes.

**Phase 5 — multi-source reads.**
New `crates/agent-share/src/mount/sources.rs`: a `SourceSet` implementing
`ByteSource`, wired into `attach` by handing `Arc::new(source_set)` to
`RemoteFs::new` instead of the bare client. `RemoteClient` itself needs **no
modification** — a peer client is the same struct with `ticket.addr` swapped for
the peer's `EndpointAddr` and the same secret. `nfs.rs` is unchanged; that is the
payoff for a well-placed seam. `watch_tree` already takes its own
`Arc<RemoteClient>` (`consume.rs:90`), so manifest authority stays pinned to the
origin for free.

Order within the phase, per the scope decision: **whole-file paths first**
(`web/src/download.ts`, plus a new `agent-share get` — `cli/mod.rs` has only
`serve`, `bench` and bare mount today), because they know the full intent and
can stripe. **NFS last**: the kernel issues 128 KiB reads (`rsize=131072`) in
app-dictated order, so multi-source there needs speculative prefetch, which
trades directly against the mount's "nothing prefetched, no disk" property.

**Phase 6 — measure.** `mount/bench.rs` and `OP_BENCH` already exist. Add a
1-origin + K-seeder scenario; plot aggregate MB/s against K. If it is not near
linear, hedging and browser multi-source are both premature.

## Edge cases and risks

1. **Mutability invalidates hashes.** `live.rs`'s 300 ms debounce rescan can
   invalidate an outboard mid-download. Consumers must tolerate both "hash
   unknown" and "hash changed under me".
2. **The direct-peer budget is shared.** Swarm sessions compete with
   `SignalAdmission` for the 16 slots; without an explicit sub-budget the mesh
   silently degrades to relay.
3. **Bootstrapping.** Until a second peer holds bytes there is no swarm. The win
   exists only after the first complete fetch — which is why phase 3 is the
   feature and 4–5 are the amplifier.
4. **Browser consumers seeding downloads.** `download.ts` pipes to a
   `showSaveFilePicker` writable: write-only, permission not surviving reload.
   Seeding a *download* means also writing to OPFS — a double write against an
   origin-scoped quota the user never agreed to. The `mount.ts` mirror path has
   no such problem. **Do not let the throughput model assume browser downloaders
   seed.**
5. **Hash privacy.** Announcing content hashes lets every mesh member correlate
   files across shares. Membership is already "everyone with the link", so this
   is not a new class of exposure, but `--no-seed` must suppress *announcing*,
   not just serving.
6. **`crates/agent-share-wasm-client/Cargo.toml` is a standalone workspace with
   its own duplicated `[patch.crates-io]`** and must move in lockstep with the
   root — a footgun already documented in its header. `bao-tree` needs no patch
   (not iroh-adjacent); `iroh-blobs` would.
7. **Honest sizing.** For the modal share — one producer, 1–5 consumers, a few
   files — swarming buys nothing; a producer's uplink is not saturated by five
   lazy mounts issuing on-demand reads. It pays on producer death, on WAN links
   (the SCTP ceiling is per-connection, so source #2 is worth ~2×), and on
   fan-out at N ≥ ~5 against a genuinely uplink-bound producer.

## Rejected alternatives

- **`iroh-blobs` as the byte plane.** It builds and pairs cleanly with the fork
  (0.103.0 wants `iroh ^1.0.0`, which rev `195cb98d` satisfies; the graph-wide
  `[patch.crates-io]` covers it, though `iroh-tickets`/`iroh-util` warrant a
  `cargo tree -d -i iroh-base` check). Its `Downloader` is genuinely good and
  its `ContentDiscovery` trait is a perfect socket for gossip-backed discovery.
  But it cannot serve a blob until it has read it in full and built its tree,
  which turns `serve` from a metadata walk into a full read of the tree —
  exactly what `manifest.rs:18-20` refuses to do, and what makes browsing a
  500 GB share while reading three files possible. It also re-hashes on every
  editor save, and its wasm store is MemStore-only
  (n0-computer/iroh-blobs#84, open). By phase 5 we have hashes, outboards,
  discovery and multi-source; what it adds beyond that is a GC/tag system and a
  redb store — optimisations on a working system, bought with ~15 new crates and
  a second ALPN. Revisit only if measurement demands it.
- **Whole-file hashes instead of bao ranges.** Cheaper, but a corrupt peer costs
  the whole file's bandwidth, bytes cannot be streamed safely to disk, and
  partial-file seeding is impossible — so a share that *is* one large file has
  exactly one seeder until the first mirror completes, i.e. it helps least
  where fan-out helps most.
- **No hashes at all** (size + mtime + tree generation only). Defensible on the
  threat model — mesh membership already requires the ticket secret, which *is*
  the full read capability, so every peer is someone you gave the link to. But
  it cannot detect plausible garbage of the right size, and it forecloses
  partial-file seeding. Rejected because fan-out on big files is in scope.
- **A write-through cache on the lazy mount** (sparse file per touched index,
  per-file complete bit, seed complete files). Buys seed-what-you-read, costs
  cache invalidation against `OP_WATCH` churn plus a partial-availability
  encoding that no longer compresses. That is iroh-blobs with a worse hash
  story. **Lazy mounts consume from the swarm; they join it only via an explicit
  `mirror`.**
- **Deterministic index re-derivation from sorted `rel_path`** — contradicts the
  append-only-with-tombstones invariant (`live.rs:1-19`) and would require the
  origin to compact on every delta, the exact silent-corruption failure that
  module exists to prevent.
- **A new `OP_READ_BY_PATH`** — new op, new wire, and `rel_path` is not stable
  either (a rename is a delete plus a create).
- **A roaring bitmap or Bloom filter of availability in the card** — 10k entries
  at 1% FP is ~12 KB, over both the frame ceiling and what belongs in a CRDT.
- **Request hedging, cross-file dedup, content-defined chunking, any ticket
  format change, any new ALPN.**

## What this still cannot do

Stated plainly so it is not discovered later:

- **No identity across processes or restarts.** Indices are a per-serve
  invariant; restart the origin and everything may be renumbered. No "resume
  this download tomorrow", no permanent link, no cross-share dedup.
- **The origin stays the manifest authority.** A share can survive its origin's
  death *frozen at the last manifest every peer saw* — a coherent, useful
  snapshot — but it cannot outlive its origin and keep mutating. A
  content-addressed root would make any seeder self-sufficient; this does not.
- **Lazy mounts consume from the swarm but never join it** without an explicit
  `mirror`.

## Verification

- **Phase 0/1:** `cargo test -p agent-share`. Run `agent-share serve` in one
  terminal and mount from a second — `Peers: 1 on mesh · 1 direct` must appear
  on **both**. Open the same share in a browser tab and confirm `TechInfo.tsx`
  shows all three on the same `tree`.
- **Phase 3 — the headline test, write it first, in
  `crates/agent-share/tests/`:** produce from a CLI, mirror in tab A, **kill the
  producer**, then confirm tab B can still mount and read the full tree from A.
- **Guard regressions, each failing-first** (each is a silent-corruption class):
  a re-seeder with a half-written mirror returns `BadIndex`, not a short read;
  a peer on a stale `tree` is never selected; a non-origin short read fails over
  rather than truncating.
- **Phase 4:** unit-test that a tampered byte from a non-origin source fails
  verification and bans that peer. Confirm `cargo build --target
  wasm32-unknown-unknown` still succeeds with `bao-tree` in the graph and that
  no `import "env"` survives — the same assertion iroh-blobs' own wasm CI makes.
- **Phase 5/6:** extend `mount/bench.rs` with the K-seeder scenario; compare
  aggregate throughput at K=1 / 2 / 4 over a delayed link (`dnctl`/`pfctl` at
  50 ms). Note `docs/research/iroh-webrtc/bench/RESULTS.md` records that a
  lossy-link run has never been performed, so this is new ground either way.
