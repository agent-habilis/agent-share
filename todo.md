# Backlog

Things found but not yet fixed. Each entry says what breaks and how it was
found, so the next person does not have to rediscover it.

## Dead-origin fallback: card sync to a fresh joiner is flaky

The seeder fallback works end-to-end — measured in real Chrome: a newcomer
against a dead origin rendered the tree and zipped all bytes out of two
seeding tabs — but not reliably. Across ~7 dead-origin connects, ~a third
failed with "no peer on the mesh vouches" after the full 30 s card wait,
while a live seeding tab sat on the same mesh with `tree` + `serving`
published. Same round, same mesh: one revival recovered fully, one expired.

Mechanism hypothesis: the joiner's mesh membership comes up, but no *live*
gossip link forms inside the wait — the rendezvous set is dominated by dead
identities (the killed producer plus every discarded revival client; each
reconnect mints a fresh endpoint), and dialing corpses eats the window. That
is the ghost-peer defect (`mesh.rs`) biting a third time: ghosts don't just
mislead the availability grid, they slow a fresh joiner's link formation.
Fixing it likely lives in fofoca (prune dead rendezvous entrants, or
prioritize recently-alive peers) rather than here; reusing one endpoint
identity across reconnect attempts (already on this list) would shrink the
corpse pool at the source.

Found by driving the full scenario in Chrome via agent-browse: seed two tabs,
kill the producer, reconnect + newcomer. `AGENT_SHARE_DISCOVERY_DEADLINE_SECS`
and the web's `origin_cap_ms` connect param exist for exactly this loop.

## Overnight `serve` at 100% CPU — the workspace shipped a leaky iroh-gossip

The 2026-08-04→05 overnight serve (debug build, quiet 3-file share) was at
100%+ CPU by morning; its recovered stderr shows 8 h of `unknown
NodeIdMappedAddr, dropped transmit` to **343 dead endpoint identities** — all
minted by one hidden browser tab whose failed reconnects create a fresh
identity per attempt (~84 s cadence).

Root cause (high confidence, A/B verification staged): the workspace resolved
the **unpatched** iroh-gossip — the connection-churn leak fix (upstream PR
n0-computer/iroh-gossip#147) is pinned by `fofoca` but `[patch]` sections are
not inherited across workspaces, so this graph never got it. Amplified by
netwatch's RTM_MISS storm (net-tools#203; each failed transmit's route miss
triggered a full interface rebuild + CoreWLAN XPC). Both pins were added to
`Cargo.toml` at 01:54 that night — two hours *after* the sick binary was
built, which is why the incident kept recurring: every long-lived serve
predated its fix. Repro on the pinned build stays at 1–2% CPU under 350-peer
churn; the unpatched-vs-pinned overnight A/B lives in
`~/Notes/projects/agent-share/runbooks/overnight-cpu/`.

Still open even with the pins: the web client should reuse one endpoint
identity across reconnect attempts instead of minting ghosts, and the CRDT
ghost-card defect (below the fold in `mesh.rs`) keeps every dead identity on
the roster forever — 100 churned consumers read as "(100 reading)".

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
  `agent-habilis-mesh` enables `fofoca-iroh-webrtc-transport/native`, and with
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
