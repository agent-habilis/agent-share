# Backlog

Things found but not yet fixed. Each entry says what breaks and how it was
found, so the next person does not have to rediscover it.

## `leave_mesh` threw a recursive-borrow panic, once, unexplained

Seen as an unhandled rejection while a revival retried against a dead producer:

```
recursive use of an object detected which would lead to unsafe aliasing in rust
```

That is wasm-bindgen's `RefCell` guard. The *escape route* is fixed —
`release()` in `web/src/App.tsx` swallowed only `owned` rejecting, so a throw
inside `leave_mesh` rejected the promise `.then` returns with nothing watching
it; both legs are now caught and logged at `console.debug`. So it can no longer
crash the page.

**Why it threw is still unknown.** `leave_mesh` is the only `&mut self` method
on `ShareClient` and it is synchronous — `spawn_local` schedules rather than
runs inline — so its `borrow_mut` should not overlap anything, and every other
method takes `&self`, where concurrent borrows are fine. Not reproduced in two
targeted attempts, including replaying the exact sequence that produced it (a
successful revival, then a dead producer, then a second kill).

Next step: a wasm panic hook that captures a Rust-side backtrace. The JS stack
was empty, which is why reading the code has not settled it.

## `cargo task ci` is red at HEAD, in two independent places

Both confirmed pre-existing by stashing all local work and re-running, so
neither is a regression — but both mean the CI gate currently cannot pass, and
anyone adding a third failure will not notice.

1. **10 clippy errors** in `crates/agent-share-proto/src/client.rs` (from
   commit `5730f722`): `doc_markdown` on "TypeScript", and `min_ident_chars` on
   a run of `|v|` / `|s|` closure parameters.
2. **6 compile errors** in `cargo test --manifest-path
   crates/agent-share-wasm-client/Cargo.toml --lib`, which `ci.rs:27-32` runs.
   `src/mesh.rs:581` and `src/produce.rs:92,390` disagree about
   `Arc<BrowserHubTransport>` vs `Arc<WebRtcTransport>`. The crate is
   wasm-only; this is its *host*-target build, which nothing else exercises.

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
  URLs make it unreachable.
