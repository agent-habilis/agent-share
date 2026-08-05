# RFC 03: `fofoca-blobs`, a storage-agnostic verified byte store

Status: **shipped, and the crate has moved.** Stage 0 executed, all five
assumptions tested; the crate was then written, and now lives in the
[`fofoca-network/fofoca`](https://github.com/fofoca-network/fofoca) workspace as
`crates/fofoca-blobs` — the network layer's other consumers want it too.
`agent-share` takes it as an ordinary dependency.

This document and its `findings/`, `data/` and `harness/` stayed here: they are
the share-side design record and the measurements behind it. The crate itself no
longer cites them — every constraint they justify is stated inline in its own
source, so it stands on its own away from this repo. Paths below that point into
`crates/fofoca-blobs/` refer to the upstream checkout.

Scope decided with the user: chunk/range serving *and* content-addressed
identity are both in scope; **browser seeding is a hard requirement**. Those two
together disqualify `iroh-blobs` and motivate a small crate of our own.

Added later, and it moved a decision: **every peer must be able to see which
chunks every other peer can seed** — the painted grid a BitTorrent client
shows. See *The availability grid*. It is the reason chunk-level availability
is a v1 concern rather than the phase-4 deferral this document originally
recorded, and the reason a third plane exists.

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

### The isolation rule

**Everything blob-shaped lives in `fofoca-blobs`. fofoca is not allowed to
learn about it.** This is a hard constraint, not a preference, and it runs in
both directions:

- **`fofoca-blobs` must not depend on `agent-share` or `agent-share-proto`.** It
  never sees a `MountManifest`, a ticket, a mesh, or an ALPN. It takes a key, a
  size, an mtime and byte ranges, and hands back verified bytes. That is what
  makes it publishable and what stops it growing into a second copy of the
  share protocol.
- **fofoca must not name a blob concept.** No `bao-tree`, no `blake3`, no
  outboard, no `ChunkRanges`, no store, in `agent-share` or
  `agent-share-proto`. Those crates keep working with the blob layer absent.

**Isolation is about modularity, not optionality.** `fofoca-blobs` is a
*required* dependency of `agent-share` — no cargo feature, no no-blobs build.
Once this ships it is the only way files move, and there is no compatibility
path back to a build without it. The crate is separate so its seam stays honest
and so it can be published on its own, not so it can be switched off.

One thing this must not be read as removing. **Lazy hashing is a runtime
property, not a compatibility shim.** RFC 01 phase 4's "a file with no hash yet
falls back to the origin" describes a file nobody has asked for yet, so nobody
has paid to hash it — which is exactly what keeps `serve` a `stat` walk and is
the whole reason we are not using `iroh-blobs`. That fallback stays. What goes
away is any notion of a peer or a build that does not understand hashes at all.

Two boundaries worth naming before they are crossed:

- **`OP_HASH`.** Wire constants are pinned in `agent-share-proto/framing.rs` by
  the `wire_constants_are_pinned` golden test, so the op *number* has to be
  reserved there. Nothing else about it does: the payload's meaning and every
  byte of its implementation belong to `fofoca-blobs`. Reserving a number is
  not learning about blobs.
- **The availability grid.** Its data comes from `BlobStore::present()`, but
  the broadcast frame, the debounce and the rendering are fofoca's. The seam is
  `ChunkRanges` in, announcement out — fofoca decides *how* to announce, blobs
  only says *what is held*.

The manifest fingerprint added in RFC 01 phase 1 is **not** blob work, despite
being a hash. It is a tree-generation marker over the manifest bytes, it uses
`sha2` which `mesh_key.rs` already pulled in, and RFC 01 guard #1 needs it
whether or not content addressing ever lands. Content addressing is BLAKE3 over
*file* bytes and lives on the other side of the line.

### The compatibility tax, and what survives it

Both RFCs were written assuming issued tickets and older peers had to keep
working. They do not. Several constraints were justified on that basis and are
now void — but some of them *also* hold for reasons that have nothing to do with
versions, and those must survive verbatim. Recorded once here so the distinction
is cited rather than re-derived, in either direction.

| Constraint | Was justified by | Verdict |
|---|---|---|
| `MountManifest::encode` frozen | "breaks every issued ticket" | **Void as stated.** The encoding may change; hashes still may not go in it, because that means hashing at scan time. |
| New ops degrade for an older producer | "costs one stream" | **Void.** `OP_HASH` / `OP_HAVE` assume a peer that understands them. |
| The ticket never changes | compat | **Void.** Spent once, on extensibility — see below. |
| Card fields optional | "old peers stay parseable" | **Reason void, shape survives.** The browser has no manifest when it joins, so `tree` has a real unknown state. |
| `from_card_value` tolerates missing fields | compat via `unwrap_or` | **Survives, restated as robustness.** A corrupt CRDT entry must cost one peer, not the roster. |
| Index stability, tombstones, append-only `LiveTree` | correctness — a stale index reads a *different file* | **Survives untouched.** Never had anything to do with versions. |
| Lazy hashing | `manifest.rs:18-20` | **Survives untouched.** Design, and the reason `iroh-blobs` was rejected. |

**The expensive mistake, stated so nobody makes it:** reading "no backwards
compatibility" as licence to simplify tombstones or index stability. Those are
*within-serve* invariants — an index that shifts under a consumer makes it read
the wrong file, in one session, with one build on both ends. `mount/live.rs`
exists to prevent that and none of it is about versioning.

**What the freedom actually bought.** Not much, and that is fine. The obvious
prize — per-file hashes in the manifest — is still forbidden by laziness. New ops
skip a degradation branch. And the ticket got one structural fix: its payload
used to end in an open-ended address field, so nothing could ever be appended;
length-prefixing it means the *next* change does not cost another break.

### Placement and dependencies

Originally `crates/fofoca-blobs/`, a normal workspace member, consumed by the
standalone `crates/agent-share-wasm-client` workspace **by path** — the pattern
`agent-share-proto`, `agent-habilis-mesh` and `fofoca-iroh-webrtc-transport`
already use (`agent-share-wasm-client/Cargo.toml:26-32`).

It has since moved to the `fofoca-network/fofoca` workspace. Both consumers
still take it by path, now across checkouts; that becomes a rev-pinned git
dependency once the move is pushed. Nothing about the seam changed — the crate
still knows nothing about a share, and its `tests/isolation.rs` now enforces
that against the network layer rather than against this repo.

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
| **Broadcast App frame** (`to: None`) | gossip fan-out, **not retained** | O(N²) per round, ≤3840 B | **the availability grid**; also rare "who has root H?" once the origin is dead |
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

**Neither would be needed in v1 for *fetching*.** Least-outstanding dispatch
discovers availability implicitly — ask, get `BadIndex` or a short read, demote.
With ~4 peers that is cheap and needs zero new protocol. Chunk queries would
earn their place only when peers hold *disjoint* chunk subsets of one large
file.

### The `HAVE?` frame, pinned (2026-08-05)

Decided with dead-origin failover (which shipped on slot-level cards alone —
seeding is whole-file today, so a chunk ask would return what the card already
says at 1 RTT instead of 0). The wire shape is fixed **now** so the day
chunk-granular seeding lands, both ends already agree; it ships together with
bao-range partial-file seeding, not before.

- **Ask** — directed app frame, `to: Some(peer)`, `corr: Some(n)`, tag
  `share/have/1`. Body, JSON: `{ "tree": "<16-hex fingerprint>", "index": n }`.
  A responder on a different tree answers `tree_mismatch` rather than ranges —
  guard #1 applies to availability answers, not only to bytes.
- **Answer** — same tag, same `corr`. Body:
  `{ "tree": "…", "index": n, "ranges": "0-511,1024-2047" }` — chunk-group
  ranges in the same sorted-RLE vocabulary `serving` uses, or `"*"` for all.
  **Advisory and coarsenable:** if the honest answer does not fit 3840 B
  signed, answer the largest prefix that does; the asker treats absence as
  "unknown", never "absent".
- Both directions ride the existing signed app-frame plane — plaintext, like
  every directed frame (`broadcast.rs:37-40`); it carries *availability*, and
  membership already implies the read capability.

The availability **grid** below changes that: it is a v1 requirement, and it
needs a third plane neither tier provides.

## The availability grid

**Requirement:** every peer can see, for every other peer, which chunks that
peer is able to seed — the painted grid a BitTorrent client shows.

This is a *display* requirement, and taking it seriously changes the design,
because neither plane above can serve it. Directed `HAVE?` covers only the peers
you chose to ask, which is ~4 of them; a grid wants all of them. And the CRDT is
still the wrong home for the reason RFC 01 gives — automerge retains history, so
a bitfield that churns as chunks arrive writes an op per change forever and
every late joiner syncs the lot.

### The third plane: ephemeral broadcast

Broadcast App frames (`to: None`), which the table above reserves for the rare
origin-dead lookup, are the right carrier. They fan out to everyone, which is
what a whole-mesh view needs, and — decisively — **they are not retained**: a
`classify()` returning `loggable: false, chained: false` keeps them out of the
message log and out of the cross-author DAG, so unlike the CRDT they leave no
history behind. This is the one genuinely broadcast-shaped use case in the whole
design.

### What makes it cheap: display resolution is not verification resolution

The obvious objection is size. At the 64 KiB chunk groups
[S0.3](findings/s03-hash-throughput.md) recommends, a 1 GiB file is 16 384
groups and a 10 GiB file is 163 840 — nowhere near the 3840-byte frame ceiling.

But **a grid does not want one cell per chunk group.** No UI usefully renders
more than a few hundred cells; BitTorrent clients bucket for exactly this
reason. So broadcast a fixed number of *display buckets* per file — say 512,
independent of file size — each holding two bits (none / partial / all). That is
**128 bytes per file** whatever the file's size, so a whole tree fits one frame,
and a complete peer sends `"*"` in one byte.

The exact ranges a *scheduler* needs stay where they were: `OP_HAVE` on an open
`MOUNT_ALPN` stream, at full chunk-group resolution, for the handful of peers we
are actually pulling from. Two resolutions, two planes, two jobs.

### Seeding in the UI

The grid above answers *what can other peers serve*. Three further requirements
answer *what can I serve, and how do I make that more*:

1. **Every file and folder shows whether it is held locally and seedable.**
2. **A sync control, top right,** fetches the whole share and makes it seedable.
3. **A sync control on a file or folder's detail view,** scoped to that subtree.

These are the same data as the grid at a different granularity — `present(root)`
per file, aggregated up a folder — so they share a source and should share a
vocabulary. Three points where the obvious reading would be wrong:

**Sync *is* `mirror`, and that is the point.** RFC 01 is deliberate that "lazy
mounts consume from the swarm but never join it without an explicit `mirror`",
because a lazy mount holds nothing durable to serve. These controls are that
explicit opt-in, given a surface. The top-right one is `syncMount` over the whole
tree; the detail-view one is the same thing scoped. Nothing starts seeding
because a user merely browsed a file.

**Three states, not two.** "Held or not" is the wrong model once bytes arrive in
ranges: a file part-fetched is *partially* seedable, and saying so is the whole
reason bao verifies ranges rather than whole files. The indicator needs
none / partial / complete, matching the grid's buckets, and a folder is
"partial" whenever its children disagree.

**"In memory" needs to mean persisted.** Bytes held only in RAM vanish on
reload, and a peer that advertises them then fails to serve is worse for the
swarm than one that never advertised. Seedable therefore means *in the store* —
OPFS in the browser ([S0.5](findings/s05-opfs-worker.md) measured it at
~1 GiB/s and confirmed it survives reload), the file in place natively. The
indicator must reflect what survives a restart, not what is currently cached.

Two consequences worth stating before they surprise someone. Sync makes a peer's
reading progress public, per the privacy note below — a user pressing it is
opting into being seen, and the control should not pretend otherwise. And a
whole-share sync on a large share is a long, resumable operation, not a click
that completes: it needs progress, cancellation, and to survive a reload, which
is what the persisted `ChunkRanges` are for.

### Costs, and the caps that follow

Every peer broadcasting to every peer is O(N²) deliveries per interval. At N=10
and a 2 s debounce that is roughly 190 KB/s of mesh traffic, which is fine; at
N=50 it is ~4.8 MB/s, which is not. Three caps, all required:

- **Broadcast on change, debounced**, never on a fixed timer. RFC 01's warning
  applies unchanged: `syncMount` writes files one at a time, so a per-file
  announce is a broadcast storm.
- **A complete peer stops announcing.** `"*"` does not change, so it is sent
  once and on join.
- **Scale the debounce floor with roster size**, so a large mesh degrades to
  coarse refreshes rather than saturating.

### The browser store, and the Worker that is not coming yet

Settled by S0.6, and the shape of the answer was not the one the design
expected.

`OpfsStore` needs a Worker, because `FileSystemSyncAccessHandle` exists nowhere
else. That was accepted as a tax until the seeding UI needed building, at which
point the tax turned out to have a second half: **`RTCPeerConnection` does not
exist in a Worker either.** Measured, not assumed — `undefined` in a
`DedicatedWorkerGlobalScope`, no prefixed alternative.

Two constraints pointing opposite ways. The data plane is pinned to the main
thread; usable OPFS is pinned to a Worker. So the lib cannot move wholesale in
either direction, and the only question is where to cut.

Cutting at the packet level — WebRTC on the main thread, everything else in a
Worker — puts every datagram through `postMessage`, on the latency-critical path
QUIC does loss recovery over. Cutting at the **storage** level puts only bulk
64 `KiB` ranges across, which was already async disk I/O. That is the cut, and
the crate was already built for it: `fofoca-blobs` is separate, `BlobStore` is
`?Send` precisely so a browser could implement it, and the blob layer weighs
347 KB against the client's 7.4 MB.

But the split is **deferred**, because a third option removes the need for it
today. `IndexedDB` addresses records rather than offsets, is reachable from any
scope, and measured linear both ways: 64 `MiB`/s write, 362 `MiB`/s read. That
is 24× slower than a Worker's sync handles and 5–50× faster than a realistic
WebRTC data channel, so on the seeding path it is not the bottleneck. Where the
Worker would win is local work — hashing a whole file, exporting one.

So [`IdbStore`](../../../../../fofoca-network/fofoca/crates/fofoca-blobs/src/idb.rs) is the browser backend,
`OpfsStore` stays unused until the Worker lands, and the trait makes that a swap.

**What was nearly built instead, and why not.** Main-thread OPFS looks viable
until measured: `createWritable` is copy-on-write, so a 64 `KiB` patch costs a
whole-file copy, and filling a file in pieces is O(n²) — about nine hours for a
gigabyte. Worth stating precisely, because the quadratic is *one operation*.
Sequential writes in a single session are linear at 566 `MiB`/s and random reads
are flat at 2.45 ms regardless of file size. Only random-access writing is
ruinous.

That distinction is what produced [`sparse`](../../../../../fofoca-network/fofoca/crates/fofoca-blobs/src/sparse.rs):
`decode_ranges` writes through `WriteAt` and `encode_ranges` reads through
`ReadAt`, so a store that keeps pieces rather than files pays the file once
rather than once per piece. It was built for `IndexedDB` and is not specific to
it — it is what *any* store that cannot patch in place needs.

### A copy must re-serve under the origin's secret, not its own

Discovered late, and the whole difference between a swarm and a chain of
unrelated shares.

`agent-share serve` mints a fresh secret, and the mesh id is derived from it. So
a mirror re-served the obvious way builds a **second** share: its own mesh, its
own ticket, and no route from the original link to it. Every requirement about
surviving the origin quietly fails — the bytes are there, on a peer nobody
holding the link can find.

So a mirror writes the secret it fetched under into its sidecar
(`origin.secret`, mode 0600), and `serve` adopts it when it finds one. The copy
lands on the same mesh, answers the same ticket, and shows up in the grid beside
the origin. Nothing in the protocol had to move for this: `produce.rs`
authenticates a read by comparing the secret and nothing else, which is the
symmetry claim S0.2 tested.

Two boundaries worth stating, because they are easy to blur:

- **The endpoint key stays fresh.** The secret is the *share* capability; the
  key is *this peer's identity*. Two peers sharing the latter would be a
  different and much worse bug.
- **The secret at rest is the read capability at rest.** Whoever ran the mirror
  already had it — it came in the link they pasted — so this stores nothing new
  to them. It does mean a mirror directory is exactly as sensitive as the link
  that made it, and `origin.secret` is why.

A truncated file is refused rather than padded. Adopting half a secret would put
the peer on a mesh nobody else is on, which presents as a share that simply has
no other peers: the worst kind of failure, because it looks like the network.

### Known defect: the grid counts peers that have left

Found while verifying the browser seeding UI, and left unfixed rather than
half-fixed.

The meta channel is a CRDT, and nothing deletes a peer's entry when it goes. A
browser tab *cannot* — `ShareClient::leave_mesh` documents that the page is torn
down before the departure broadcast runs. So the document accumulates every peer
that has ever joined.

That was harmless while a card was a display label. It stopped being harmless
once cards carry `serving`, because the grid aggregates across peers to answer
**"which slots would be lost if these peers went away"** — and a departed peer's
slots make it answer *none*. Reloading one tab three times reproduced it: five
availability rows against `2 on mesh`.

**The obvious fix was tried and reverted.** Filtering the card book against
`roster_snapshot()` made a *live* producer's card vanish — book and roster are
fed by different events, so whichever lands second leaves the other stale, and a
card that arrives before its roster entry is filtered out as a stranger with no
later meta change to bring it back. Rebuilding on `on_tick` did not rescue it,
and the reason the roster looked empty was not established.

Showing a peer that is gone is a wrong answer; hiding a peer that is there is a
worse one. So the ghost stays until the ordering is understood, pinned by
`mount::mesh::tests::a_peer_that_left_is_still_listed` so it reads as a known
defect rather than as intended behaviour. The real fix is probably to delete the
departed peer's CRDT entry on `on_peer_left` rather than to filter on read.

### Two things this must not be mistaken for

**The grid is advisory, not authoritative.** A painted cell means "that peer
said so, up to a debounce ago, at bucket resolution". A scheduler that treats it
as fact will ask for chunks a peer does not have. That is already handled —
least-outstanding dispatch with demote-on-failure — but the grid must not be
wired into fetch decisions as though it were exact.

**It publishes reading progress to the whole share.** RFC 01 risk #5 notes that
announcing content hashes lets members correlate files; a broadcast availability
map is strictly more than that, since it says what each peer has fetched so far,
continuously, to everyone holding the link. Membership already implies the full
read capability so this is not a new class of exposure, but it is a new
*degree*, and `--no-seed` must suppress announcing as well as serving.

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
  on it. Second kill-gate, from *The isolation rule*: the crate must compile
  with no dependency on `agent-share*` at all. If the trait needs a
  `MountManifest` to be useful, the seam is in the wrong place.
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
- **Do not put hashes in `MountManifest`.** The encoding is no longer frozen, so
  the reason is not compatibility: filling that field means hashing at scan
  time, and `manifest.rs:18-20` refuses exactly that. See *The compatibility
  tax, and what survives it*.

### Stage 3b — seeding, and seeing who seeds

Independent of the byte plane, and shippable as soon as a peer has ranges worth
announcing. Two halves that share a data source; the local half is the one
users act on, so build it first.

**3b-i — local seeding state and the sync controls.** Per-file and per-folder
none/partial/complete from `present(root)`, the top-right whole-share sync, and
the scoped sync on a detail view. No mesh traffic at all: this is a peer looking
at itself.

- **Depends on** `BlobStore::present()` (stage 2) and the existing mirror path
  (`web/src/mount.ts::syncMount`, `produce.ts::scanDirectory`).
- **Kill-gate:** press whole-share sync, reload the tab, and the indicators come
  back complete. If they do not, the bytes were not in the store and the peer
  would have been advertising what it cannot serve.

> **BLOCKED, and by the thing this document already predicted.** `OpfsStore`
> needs a `DedicatedWorkerGlobalScope` — sync access handles do not exist
> anywhere else — and **the wasm client runs on the main thread**. There is no
> `Worker` in `web/src` at all, and the client reaches for `web_sys::window()`
> in three places: `lib.rs:1245`, `produce.rs:891`, and
> `fofoca-iroh-webrtc-transport/src/web/jsep.rs:604`, each a `setTimeout`.
>
> *The Worker convergence* above says to design the browser backend around one
> Worker "from the start rather than retrofitting it". This is the retrofit
> arriving, and it is a prerequisite rather than a detail:
>
> - **Move the whole client into a Worker.** Correct end-state — hashing leaves
>   the main thread too, which is the other half of the convergence. Costs
>   moving `WebRTC` and the mesh in with it, and the three `setTimeout` sites
>   become `WorkerGlobalScope::set_timeout_*`.
> - **A store-only Worker**, with the client staying put and talking to it by
>   `postMessage`. Smaller, but hashing stays on the main thread — so a
>   multi-gigabyte sync janks the UI, which the convergence argument says is
>   the thing to avoid.
> - **Give up random access on the main thread**, using the async
>   `createWritable` API instead. No Worker, but no writing at an offset
>   either, so a mirror could not fill a file from several peers — which is
>   most of the point.
>
> The first is the one the design wants. None of them is a small change, and
> the native side is unblocked either way: `FsStore` needs no Worker, so a CLI
> `agent-share mirror` can seed today.

**3b-ii — the availability grid.** Broadcast the same state, bucketed, on the
app-frame plane; render every peer's.

- **Depends on** 3b-i, and on nothing else. It does **not** need multi-source
  reads.
- **Kill-gate:** a peer mirroring a large file shows a grid that fills in as it
  fetches, and a complete peer shows a full one after a single announce. Mesh
  traffic stays flat as the mirror progresses — proving the debounce holds and
  a per-file announce did not slip in.
- **Watch for:** the O(N²) broadcast cost. Measure with a roster of 10 before
  assuming the caps are enough.

### Stage 4 — multi-source reads (RFC 01 Phase 5)

Whole-file paths first (`web/src/download.ts`, a new `agent-share get`), **NFS
last**. `sources.rs` consumes `BlobStore` but still owns peer selection.

The grid from stage 3b is **not** an input here. It is bucketed and up to a
debounce stale; the scheduler uses exact `OP_HAVE` ranges from peers it has
connected to.

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

**The isolation rule, as a command.** Nothing blob-shaped may appear in fofoca
core:

```
rg 'bao_tree|blake3|BlobStore|ChunkRanges' crates/agent-share crates/agent-share-proto
```

Empty until `fofoca-blobs` exists, and afterwards only ever matching through
that crate. **Match those symbols, never the bare word `blob`** — three
unrelated things in this repo answer to it, and only the third is ours:

| Where | What it is | Ours? |
|---|---|---|
| `agent-habilis-mesh/src/blob/` | The vendored engine's *blob channel*: point-to-point transfer of gossip payloads too large for a frame, SHA-256 addressed, its own ALPN and ticket. Declared `pub(crate)`, so it is unreachable from `agent-share` and cannot collide in code. | no |
| `web_sys::Blob` | The browser's file object — `blob.array_buffer()` in `agent-share-wasm-client/src/produce.rs`. This is the one that will genuinely share a file with `OpfsStore`. | no |
| `fofoca_blobs::BlobStore` | This crate. | yes |

The name was kept deliberately after checking: the mesh module is private and
the browser type is a different Rust type, so neither collides structurally.
Renaming the vendored module was rejected — its name is upstream's, and this
fork re-syncs by diffing against `8914557`, so churning it would make every
future re-sync noisier for a purely local preference.
