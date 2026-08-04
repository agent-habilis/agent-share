# S0.6 — a browser store with no Worker

**Verdict: `IndexedDB` works, main-thread OPFS does not.** Raw capture:
[`../data/s06-idb-safari.txt`](../data/s06-idb-safari.txt).

## Why it mattered

S0.5 established that OPFS random access needs a Worker. That was accepted as a
tax until the seeding UI came to be built, at which point the tax turned out to
have a second half: **`RTCPeerConnection` does not exist in a Worker.** Probed
directly — `undefined` in a `DedicatedWorkerGlobalScope`, in Safari, with no
vendor-prefixed alternative.

So the two constraints point opposite ways. The data plane is pinned to the main
thread; usable OPFS is pinned to a Worker. Neither the whole lib nor the whole
store can move, and something has to give.

The question this spike answers: is there a browser store that is persistent,
linear, and reachable from the thread `RTCPeerConnection` already lives on?

## What was established

**[measured] Main-thread OPFS is quadratic, and it is one specific operation.**
`createWritable` is copy-on-write in Safari, so a small patch costs a whole-file
copy:

| file | 64 KiB patch |
|---|---|
| 1 MiB | 8 ms |
| 8 MiB | 25 ms |
| 64 MiB | 117 ms |

Filling a file in 64 KiB pieces is *n* pieces each costing O(*n*): 64 MiB takes
~120 s, 1 GiB about 8.5 hours.

**[measured] But only that operation.** The penalty is per `createWritable()`
*open*, not per write:

| operation | cost | scaling |
|---|---|---|
| sequential write, one session | 250 → 566 MiB/s | linear |
| random 64 KiB read via `slice()` | 2.45 ms, flat at 1 MiB and 64 MiB | independent of size |
| random 64 KiB write, own session | 117 ms @ 64 MiB | O(*n*) each |

Worth recording because the first reading was wrong: an earlier 217 ms "read"
was `getFile()` timed inside the loop, which is once per handle, not once per
read. Reads were never a problem.

**[measured] `IndexedDB` is linear both ways**, addressing records rather than
offsets. 64 MiB as 1024 × 64 KiB records, shuffled, one transaction: **64 MiB/s
write, 362 MiB/s read**. About 24× slower than a Worker's sync handles, and
5–50× *faster* than a realistic WebRTC data channel — so on the seeding path it
is not the bottleneck. The Worker would win on local work: hashing a whole file,
exporting one.

**[measured] `IdbStore` passes its conformance checks in a real tab**, twice —
once fresh, once after a reload. Nine properties including partial serving,
refusal of an absent range, the mtime version gate, tamper rejection, and
persistence.

## What this would have broken

Two bugs, neither of which a compile check could see. Both were found by running
the store rather than by reading it.

**The outboard was never persisted.** Every other backend rebuilds it from the
whole file on read, so none of them store one. The sparse path cannot — it reads
only the blocks it was asked for, and has nothing to rebuild from. The nodes
exist only in the verified stream that proved them. `decode_sparse` now takes
the outboard by `&mut` and carries it across calls. Symptom was "bound to a root
whose outboard is gone", which reads like corruption and was a missing write.

**Blocks were keyed by file, range sets by root.** Two files with identical
content share a root, so completing one made the other advertise ranges whose
blocks did not exist — a store reporting a file complete and then failing its
first read. Fixed by keying blocks by root as well, which deduplicates identical
content as a side effect.

That second one is **latent in `OpfsStore` for the same reason**. It surfaces
there as an error rather than as corruption, because those backends read the
file at `file.key` and fail when it is short. Worth fixing when the Worker split
lands rather than rediscovering.

## What is still assumed

- **Chrome.** Every number here is Safari on one machine. Copy-on-write is an
  implementation choice; Chrome may have no such cliff, in which case the
  main-thread OPFS option reopens. Carried over from S0.5, still open.
- **Eviction.** Neither store was pushed to a quota limit. A peer that
  advertises ranges the browser later reclaims looks to everyone else like a
  peer that lies. `present` reads the range set back from storage rather than
  caching it, which is the mitigation, but the failure mode is unmeasured.
- **Concurrency.** One tab. Two tabs on the same share would open the same
  database, and nothing here tests what they do to each other.

## What it changed

`IdbStore` is the browser backend for now, and the Worker split is deferred
rather than blocking. `OpfsStore` stays in the crate, unused, for when it lands
— the `BlobStore` trait is what makes that a swap rather than a rewrite, and is
the reason its futures are `?Send`.

The sparse path (`SparseBlocks`, `encode_from_outboard`, `decode_sparse`) was
built for `IndexedDB` but is not specific to it. It is what any store that
cannot patch bytes in place needs, and it is what makes accepting a file in
pieces cost the file rather than the file squared.
