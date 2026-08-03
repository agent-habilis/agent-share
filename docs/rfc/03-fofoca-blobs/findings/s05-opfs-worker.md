# S0.5 — OPFS random access from a Worker

**Verdict: holds in Safari; Chrome unverified.** Raw capture:
[`../data/s05-opfs-safari.txt`](../data/s05-opfs-safari.txt).

## Why it mattered

This was the biggest unknown going in, and the one that could have killed the
crate. Browser seeding is a hard requirement, and it is the reason `iroh-blobs`
is disqualified (its wasm store is `MemStore`-only). If `fofoca-blobs` could not
persist ranges in the browser either, it would inherit the same defect and its
main advantage would evaporate.

Concretely: does `FileSystemSyncAccessHandle` — the only OPFS API with
read/write at an arbitrary offset — work from a Worker, survive a reload, and go
fast enough to be a store backend?

## What was established

**[measured]** Safari 27, 64 MiB written in shuffled 64 KiB chunks:

| Operation | Rate |
|---|---|
| Random-access range writes | **955–1561 MiB/s** |
| Random-access reads | **4267–9143 MiB/s** |

Zero mismatches in every run, correct final file size, 76.8 GiB quota.
**Persistence across a full page reload confirmed.**

The write order is deterministically shuffled, not sequential — a real seeder
receives ranges from several peers in whatever order they arrive, so that is the
access pattern under test.

**[verified]** `web-sys` 0.3.103 exposes the bindings Rust needs:
`create_sync_access_handle`, `read_with_u8_array_and_options` and
`write_with_u8_array_and_options` (both taking an `at` offset), `flush`,
`get_size`, `close` — from `DedicatedWorkerGlobalScope`. Compile-checked against
`wasm32-unknown-unknown`.

**OPFS is not the bottleneck.** At ~1 GiB/s writes against ~2 GiB/s hashing,
the store will wait on BLAKE3, not on storage.

## The Worker constraint became an asset

**[verified]** `createSyncAccessHandle` is absent on
`FileSystemFileHandle.prototype` in **window** scope and present only in a
Worker — confirmed empirically in Safari, where the window-scope probe returns
`false`. This is per spec, not a Safari quirk.

Going in, this was recorded as the design's biggest risk. It is not, because
hashing a multi-gigabyte mirror would jank the main thread anyway. **One
dedicated Worker owns both OPFS handles and all hashing** — the constraint and
the performance requirement have the same solution. Design the browser backend
around that Worker from the start rather than retrofitting it.

## Method note: two test bugs that looked like OPFS failures

Both initially reported `FAIL` and neither was real. Recorded because they are
the kind of mistake that produces a confident wrong verdict:

1. **The persistence marker lived at offset 0 of the same file the
   random-access test wrote to**, so the shuffled writes clobbered it every run.
   Fixed by giving the marker its own file.
2. **The size assertion compared against the current run's requested size**, but
   a previous run had left the file larger. Fixed with `truncate(0)`.

The evidence that persistence worked *even before the fix* was in the data: the
second run reported `size=67108864` (64 MiB, from the previous page load) while
writing only 16 MiB, and quota usage went 0.00 → 0.06 GiB across the reload. The
file had plainly survived; the test was just asking the wrong question.

## Why the browser half is plain JS

Deliberate split. The behavioural question — does OPFS random access work,
persist, and how fast — is answered in plain JS in a Worker, with no dependency
on a `wasm-bindgen` CLI version match. The Rust question — are the bindings
reachable at all — is answered independently by a headless compile check.

Two small robust experiments beat one large fragile one.

## Still open

- **Chrome.** Sync access handles are Worker-only in both engines, but quota
  policy and eviction behaviour differ, and Chrome is where most users will be.
  Cheap to close: serve the harness over `http://localhost` (a secure context),
  run it, reload, run again.
- **Eviction under pressure.** OPFS is origin-scoped and evictable. Nothing here
  tests what happens when the quota fills or the browser reclaims storage — a
  seeder that silently loses ranges it advertises is a correctness problem, not
  just a performance one. `navigator.storage.persist()` is the mitigation and is
  untested.
- **Concurrent handles.** Only one sync access handle per file may be open at a
  time. A design with several readers plus a writer needs to serialise through
  the one Worker, which is the plan, but has not been exercised.
