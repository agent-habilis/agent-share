# Plan: live browser shares — rescan + real OP_WATCH + resilient consumer watch

Implementation plan for a fresh agent. Self-contained: findings, design
decisions, steps, edge cases, verification. No work has been started.

## Context: what is broken and why

Sharing a folder is supposed to be live (adds/edits/deletes on the producer
reach receivers without remount — see `README.md` and commit ff93f3a). The
native CLI producer is live; **sharing from the browser never was**:

1. **One-time scan.** `ui/src/produce.ts:57-84` walks the picked directory
   once, calls `handle.getFile()` per file, and keeps only the resulting
   `File` snapshots — both the per-file `FileSystemFileHandle`s and the root
   `FileSystemDirectoryHandle` are discarded. Nothing can ever re-scan.
2. **Frozen tree.** `crates/agent-share-wasm-client/src/produce.rs:83-86`
   freezes everything into an immutable `Arc<ServeTree>`; the manifest is
   built once at `:57-77` with hardcoded modes and `mtime: 0`.
3. **OP_WATCH is a stub.** `produce.rs:596-615` writes exactly one
   `WATCH_FRAME_MANIFEST` then parks on `std::future::pending::<()>().await`
   (also leaking a task per subscriber). Receivers never get an update.
4. **Edited files hard-fail reads.** Chrome invalidates a `File` snapshot
   when the backing file changes on disk, so `answer_read`
   (`produce.rs:642-669`) gets a rejected `array_buffer()` promise
   (`NotReadableError`) → `ReadStatus::Io`. The receiving browser escalates
   any read error during mirror-sync into a full unmount
   (`ui/src/App.tsx:414-419`), so one edited file tears the receiver down.
5. **Consumer watch is one-shot.** The browser consumer's subscription
   (`crates/agent-share-wasm-client/src/lib.rs:143-191`) permanently ends on
   ANY error — read, decode, serde, or a throwing JS callback at `:184` —
   and invisibly (the fn returns Ok right after `spawn_local`; `App.tsx:436`
   can never observe the death). Compare the native consumer, which retries
   every 3 s (`crates/agent-share/src/mount/consume.rs:142-201`).

Scope decided with the user: **fix browser both sides** (producer liveness +
consumer watch resilience). Native watch fixes (silent stream-death treated
as EOS, uncapped debounce) are known but deferred.

## Load-bearing facts about the existing wire/consumers

- Watch stream frames: `status(1) ‖ len(u32 LE) ‖ body`, where `body[0]` is
  `WATCH_FRAME_MANIFEST = 0` (full manifest follows) or
  `WATCH_FRAME_DELTA = 1`. Constants in `agent-share-proto`.
- **Both consumers accept a full-manifest frame at any time** (native:
  `consume.rs:187`; wasm: `lib.rs:170`), so the browser producer never needs
  delta computation — full-manifest frames only.
- **Index stability is the READ-address invariant.** Native `LiveTree`
  (`crates/agent-share/src/mount/live.rs:161-263`) keeps
  `index_of: HashMap<path, u32>`, reuses a path's slot, appends new files,
  and tombstones vanished slots (`FileEntry::tombstone()`, empty rel_path).
  `ui/src/tree.ts:110` skips tombstones; `ui/src/mount.ts:134-141` diffs per
  file on `(size, mtime, index)`.
- `FileEntry.mtime` is **seconds** since epoch (`manifest.rs` doc); browser
  `File.lastModified` is milliseconds — divide by 1000 or native NFS
  consumers show year-55k dates.
- `ShareClient` holds only the live `Connection` (which is `Clone`) — the
  ticket is consumed at connect. Reconnecting the WebRTC path means redoing
  the two-connection JSEP dance, so consumer watch retry must reuse the
  existing connection and give up when `conn.close_reason().is_some()`.
- wasm is single-threaded and `FileSystemFileHandle` is `!Send`: state goes
  in `Rc<RefCell<…>>`, never `Arc<Mutex<…>>`, and **no borrow may be held
  across an await**.

## Step 1 — `crates/agent-share-wasm-client/src/live_state.rs` (new)

Port of `LiveTree::apply` (`live.rs:161-263`) minus tokio/notify/PathBuf,
generic over the slot payload so it unit-tests on the host (the crate is an
`rlib`; see the `transport_mode` module for the precedent):

```rust
pub(crate) struct LiveState<T> {
    dirs: Vec<DirEntry>,
    files: Vec<FileEntry>,          // wire order, tombstones in place
    slots: Vec<Option<T>>,          // index-aligned; None where tombstoned
    index_of: HashMap<String, u32>, // every path ever assigned, forever
    encoded: Vec<u8>,               // re-encoded once per change batch
}
impl<T> LiveState<T> {
    pub fn new(dirs: Vec<DirEntry>, files: Vec<(FileEntry, T)>) -> Self;
    /// Fold a rescan in. Returns true when anything changed.
    pub fn apply(&mut self, dirs: Vec<DirEntry>, files: Vec<(FileEntry, T)>) -> bool;
    pub fn encoded(&self) -> &[u8];
    pub fn slot(&self, index: u32) -> Option<&T>; // None = tombstone/bad index
    pub fn live_counts(&self) -> (u32, u64);      // files, bytes, sans tombstones
}
```

Same semantics as native: dirs replaced wholesale and diffed by path; files
upsert into their owned slot or append; slots not seen this scan are
tombstoned; unchanged test = same `(size, mode, mtime)` and not a tombstone.
Dedupe duplicate `rel_path`s first-wins before applying. Port the `live.rs`
unit tests (e.g. `a_removed_file_leaves_its_slot_alone`) with `T = ()`.

Optional later refactor (NOT now): hoist into `agent-share-proto`, have
native `LiveTree` wrap it.

## Step 2 — producer, `crates/agent-share-wasm-client/src/produce.rs`

- State:
  ```rust
  struct ProducerShared {
      state: LiveState<web_sys::FileSystemFileHandle>,
      watchers: Vec<futures::channel::mpsc::UnboundedSender<Rc<Vec<u8>>>>,
  }
  type Shared = Rc<RefCell<ProducerShared>>;
  ```
  replaces `Arc<ServeTree>`. Delete `ServeFile`, `ServeTree`, and the
  bespoke `impl Clone for ServeFile` (`produce.rs:~468-476`). `files`/`bytes`
  getters read `live_counts()` instead of frozen fields.
- `parse_listing` (`produce.rs:416`): entries become
  `{rel_path, size, mtime, handle}`; `dyn_into::<FileSystemFileHandle>()`;
  keep the existing `safe_rel_path` filtering (it already rejects the empty
  path, so no collision with tombstones); build `FileEntry` here with the
  real mtime (removes the `mtime: 0` manifest block at `:57-77`).
- New method — **sync on purpose** (no awaits ⇒ atomic wrt reads and watch
  registration):
  ```rust
  pub fn update(&self, listing: JsValue) -> Result<(), JsValue>
  ```
  Parse → `borrow_mut` → `apply()`. If changed: refuse (warn, keep previous
  state) when the new encoding exceeds `MAX_MANIFEST_BYTES`; else build one
  `Rc<Vec<u8>>` frame (`WATCH_FRAME_MANIFEST byte ‖ manifest bytes`) and
  `watchers.retain(|tx| tx.unbounded_send(frame.clone()).is_ok())` — dead
  subscribers self-prune.
- `OP_WATCH` handler (replaces the stub at `:596-615`): in a single borrow,
  create the unbounded channel, push the sender, snapshot `encoded` for the
  opening frame; drop the borrow; write the status byte + opening frame;
  then `while let Some(frame) = rx.next().await` write `len(u32 LE) ‖ frame`
  per frame (matching what both consumers' header-driven reads expect —
  compare `serve_watch` in `crates/agent-share/src/mount/produce.rs:267-289`
  for the exact native shape). Break on any write error. This also fixes the
  `pending()` task leak.
- `answer_read` (`:642-669`): resolve `state.slot(index)` under a short
  borrow, clone the handle, drop the borrow, then
  `JsFuture::from(handle.get_file()).await` for a **fresh `File` per read**;
  clamp to the fresh `File.size()`; slice as today. On `get_file()` failure,
  re-borrow, re-fetch the slot (an update may have installed a fresh
  handle), retry once; then `ReadStatus::Io`. Tombstoned/out-of-range →
  `BadIndex`. While here: stop cloning the whole file vec per stream
  (`produce.rs:558` / `:319-326`) — pass the `Shared` handle down instead
  (`serve_mount`/`serve_stream`/`accept_loop` signatures change).
- `stop()` unchanged beyond what exists (endpoint close kills connections →
  watch writes fail → loops end; `hub.detach_all()` already landed).
- `Cargo.toml` (this crate's own web-sys feature block, lines ~38-43): add
  `"FileSystemHandle", "FileSystemFileHandle"`. Do NOT touch
  `fofoca-iroh-webrtc-transport`'s web-sys block.

## Step 3 — producer JS, `ui/src/produce.ts`

- `scanDirectory` keeps handles:
  `{rel_path, size, mtime: Math.floor(file.lastModified / 1000), handle}`;
  drop the `File` from the listing (it still calls `getFile()` for
  size/mtime).
- `startProducer`: retain the root `FileSystemDirectoryHandle`; after
  `ShareProducer.start(listing)`, run a **chained-timeout loop** (NOT
  `setInterval` — a slow walk must not overlap itself):
  ```ts
  const POLL_MS = 2000
  let stopped = false
  ;(async () => {
    while (!stopped) {
      await sleep(POLL_MS)
      try {
        const listing = await scanDirectory(root)
        if (!stopped) producer.update(listing)
      } catch (error) {
        console.warn('[share] rescan failed; serving previous tree', error)
      }
    }
  })()
  ```
  The catch covers permission revocation / root deletion mid-share (walk
  throws `NotAllowedError`/`NotFoundError` → keep serving the last tree) AND
  the stopped-while-polling race (`stop(self)` consumes the wasm object; a
  late `update()` throws a neutered-pointer error). The wrapper's `stop()`
  sets `stopped = true` **before** calling `producer.stop()`.
- Known cosmetic limitation, do not fix now: the Home screen snapshots
  file/byte counts at share start and won't re-render live counts.

## Step 4 — consumer, `crates/agent-share-wasm-client/src/lib.rs` `watch()`

Restructure into an outer retry loop; JS API shape unchanged
(`await client.watch(cb)` — `App.tsx` needs no edits):

```rust
pub async fn watch(&self, on_manifest: js_sys::Function) -> Result<(), JsValue> {
    let conn = self.connection.clone();
    let secret = self.secret;
    wasm_bindgen_futures::spawn_local(async move {
        loop {
            match follow_watch(&conn, &secret, &on_manifest).await {
                WatchEnd::Unsupported => return,       // clean end, zero frames
                WatchEnd::Retryable => {
                    if conn.close_reason().is_some() { return }
                    wait_ms(3_000).await;
                }
            }
        }
    });
    Ok(())
}
```

`follow_watch` = the current `:154-189` body (open bi-stream, send the
OP_WATCH request, frame loop) with three behavior changes:

1. Each attempt re-requests `OP_WATCH`, so every retry gets a fresh
   full-manifest opening frame — state resync is free.
2. Classify ends: decode/serde failure or mid-frame truncation →
   `Retryable`; a clean stream end **before the first frame** →
   `Unsupported` (prevents retry-spinning against a producer that doesn't
   do live watch).
3. A throwing JS callback → `console.warn` + continue, instead of ending
   the subscription.

Optional cheap addition: a second `Option<js_sys::Function>` status callback
(`"live" | "retrying" | "ended"`); leave `App.tsx` unwired for now.

## Step 5 — mirror resilience, `ui/src/mount.ts`

In `syncMount` (`:185-190`), wrap the per-file `writeFile` in try/catch: on
failure `console.warn`, delete that path from `nextFiles` (so the next pass
retries it), continue. Throw `MountError` only if EVERY file of a non-empty
write batch failed — total failure means the mount root itself is
gone/revoked, and `App.tsx:414-419`'s unmount is then the right outcome.
`App.tsx`'s `syncing`/`syncDirty` coalescing already handles watch frames
landing mid-sync; no changes there.

## Edge cases (all must hold)

| Case | Handling |
|---|---|
| Share stopped while polling | `stopped` flag + catch around neutered-wasm `update` |
| `update` during in-flight read | handle cloned pre-await; index stability; one retry via re-lookup |
| Duplicate rel_paths in a listing | first-wins dedupe in `LiveState::apply` |
| Permission revoked / root deleted | walk throws → keep last tree; reads → fresh-`getFile` failure → per-file `Io`, no unmount (Step 5) |
| Manifest outgrows `MAX_MANIFEST_BYTES` via churn | `update` refuses, keeps serving previous state |
| Consumer callback throws | warn + continue |
| Producer without live watch (old/stub) | clean zero-frame end → consumer stops, no spin |

## Verification

- Host tests: `cd crates/agent-share-wasm-client && cargo test` — the new
  `live_state` suite (ported `live.rs` tests + dedupe + oversize-refusal).
- `cd ui && bun run typecheck && bun test`.
- Build: `cargo task web-wasm` (regenerates `dist/{web,nodejs}` that
  `ui/src/produce.ts` imports; needs wasm32 target + wasm-bindgen CLI +
  Homebrew LLVM clang). Native suites must stay green:
  `cargo test -p agent-share` (note: `relay_mode_rejects_loopback_ticket_...`
  has a pre-existing environment-dependent failure, not a regression) and
  `cargo test -p fofoca-iroh-webrtc-transport --features host`.
- Manual (required — `showDirectoryPicker` needs a user gesture; dev server:
  `cd ui && bun run dev` → localhost:5173): tab A shares a folder, tab B
  joins via the ticket URL. Then:
  - edit a file → tab B's tree updates within ~2-4 s and a download serves
    the NEW bytes (this was the `NotReadableError` repro);
  - add + delete files → appear/disappear, no index shifts (mirror-mount
    rewrites only changed files, deletions propagate);
  - revoke tab A's folder permission → tab B keeps the last tree, per-file
    read failures do NOT unmount;
  - stop the share in tab A → tab B's watch ends quietly.
  - Also join a native `agent-share serve <dir>` share from the browser and
    edit files natively — exercises the consumer retry loop and the native
    producer's delta frames.
