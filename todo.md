# Backlog

Things found but not yet fixed. Each entry says what breaks and how it was
found, so the next person does not have to rediscover it.

## Nothing observably breaks when the zip entries are built eagerly

`web/src/download.ts` builds its ZIP entries from a generator, and the comment
there says an eager `.map()` "would have stalled on the first tick" past the
producer's 100-stream ceiling. `cargo task e2e` says otherwise: reverting to
`.map()` passed `web-download-zip` at 301 files and again at 1201. quinn queues
stream opens beyond `max_concurrent_bidi_streams` rather than refusing them, so
the reads drain and the archive completes.

So the lazy form is hardening, not a fix for an observed failure — worth either
finding the input that does break it (a share whose per-file reads are slow
enough that 100 in flight cannot drain?), or softening the comment's claim. The
lazy version is still the right shape; only the justification is overstated.

## The reconnect budget overruns in a hidden tab

`RECONNECT_TIMEOUT_MS` is 60 s, but a tab in the background gives up at
110–123 s. The browser throttles the give-up timer itself — measured stretching
`setTimeout(1000)` to 13–20 s intervals after about ten seconds hidden — so the
deadline fires late by exactly the amount the clock is being starved.

Bounded and terminating, so not urgent, but the constant does not mean what it
says. A monotonic check against `Date.now()` on each poll tick would honour it
regardless of timer drift.

---

## Fixed since this file was written

- **`cargo task ci` being red at HEAD**, in two independent places, both
  pre-existing rather than regressions. The clippy errors in
  `agent-share-proto` were `doc_markdown` on "TypeScript" and `min_ident_chars`
  on a run of `|v|`/`|s|` closure parameters; two more of the same kind turned
  up elsewhere once the first crate compiled far enough to reveal them. The
  wasm client's *host*-target lib tests could never have compiled: off wasm32
  `agent-habilis-mesh` enables `fofoca-iroh-webrtc-transport/host`, and with
  both backends on `WebRtcHandle` takes an `Arc<WebRtcTransport>` while the
  client hands it an `Arc<BrowserHubTransport>`. Those 15 tests now run on
  wasm32, where the crate actually builds.

- **The dev server serving a stale `.wasm`.** `dev.ts` opened `Bun.file` once at
  module load, so a `cargo task web-wasm` after startup was never picked up —
  measured serving 7,382,976 bytes against 7,383,873 on disk, which surfaced as
  `CompileError: … Custom section … would overflow Module's size`. Now opened
  per request and served `no-store`.
- **A deploy breaking the app for returning visitors.** The wasm shipped at a
  fixed filename while Bun content-hashed the JS chunks, so new glue met a
  cached old module — a hard instantiate failure on a static host with nothing
  to patch it. `build.ts` now content-hashes the wasm and rewrites the path in
  the emitted chunks, failing the build loudly if that literal ever disappears.
- **Safari caching the bad copy hard.** A consequence of the two above; hashed
  URLs make it unreachable. Note the dev-server fix does *not* retroactively
  evict what Safari already stored — an entry cached before `no-store` existed
  keeps being served. `fetch(url, { cache: 'reload' })` from the console
  replaces it without emptying the whole cache by hand.
- **`leave_mesh` throwing wasm-bindgen's recursive-borrow panic.** Fixed
  structurally rather than empirically: it was never reproduced in four
  targeted attempts, but the hazard is provable from the signatures, since
  wasm-bindgen holds an object borrowed for the entire lifetime of an async
  `&self` future and `ShareClient` has four such methods. `mesh` moved behind a
  `RefCell` so `leave_mesh` takes `&self`, leaving the type with **no `&mut
  self` methods at all** — which is the invariant, and is checkable by
  inspection even though the symptom never was.
