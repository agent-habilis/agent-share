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

Mechanism, now **confirmed in fofoca source**: the producer is the mesh's
*beacon*. Joiners bootstrap by dialing the seed-derived rendezvous identity
homed at the relay, and that identity is bound by whichever member claims the
beacon — always the share's first member, i.e. the native producer
(`CoHostPolicy::Deferred` + `BEACON_COHOST_GRACE_SECS = 10`). Kill it and the
mesh's front door goes dark: surviving *meshed* tabs only re-probe the vacancy
on the heal cadence (`RIVAL_RECHECK_MESHED_SECS = 300` plus up to 300 s
jitter), and a fresh joiner claiming after its own 10 s grace becomes a lone
island whose merge rides the same slow cadence. Failover therefore lands in
1–10 minutes, and any client-side wait shorter than that reads as "the share
is gone". The web app now simply outlasts it: it retries forever with one
persistent mesh membership (`WAITING_MESHES` in the wasm client). The real
fix is fofoca-side and still open: **event-driven beacon failover** — probe
the rendezvous when the link to the beacon dies rather than on the 300 s
cadence — plus pruning dead rendezvous registrants (the corpse's relay
registration lingers and makes vacancy probes read "held").

Second-order effect, observed in the 21:40 drill: a joiner that lands during
a dark window claims the rendezvous itself and becomes a **lone island**; the
established island only merges into it on the same slow cadence, so that tab
waits tens of minutes even after the seeders stabilize. Client-side fix worth
doing regardless of fofoca: a reviving tab already *knows* the endpoints of
the seeders it lost (its dead connection, the vetted cards) — **redial those
addresses directly** instead of waiting for the mesh to reintroduce everyone.
The bytes plane never needed the beacon; only discovery does.

Found by driving the full scenario in Chrome via agent-browse: seed two tabs,
kill the producer, reconnect + newcomer. `AGENT_SHARE_DISCOVERY_DEADLINE_SECS`
and the web's `origin_cap_ms` connect param exist for exactly this loop.

## Producer-less recovery works but is slow: ~60 s to re-arm, ~80 s to join

Measured in the 2026-08-07 drill (pinned rev `52a72f76`, raced candidate
loop in place): producer SIGKILLed, both seeder tabs hard-reloaded — each
took ~60 s to come back ready, and a fresh newcomer took ~80 s to render the
tree. Everything worked; nothing was fast. The console shows where the time
goes: a full patient origin dial against a corpse, then the card wait, then
the JSEP round — serial stages, each sized for the worst case, all paid on
the happy path too.

Levers, none needing fofoca changes: a reloaded seeder tab *knows* the
origin from its sidecar era — pass the tight `originCapMs` the revival path
already uses instead of the patient default; a tab re-armed from storage
could render its tree offline immediately (the open item above) and let the
mesh catch up underneath; and the newcomer's origin dial could concede as
soon as the first vouching card lands instead of running its full budget —
cards arriving are proof the share moved on. Worth re-measuring after each:
the drill is reproducible end to end with the picker stubbed to OPFS.

## Fixed since: refreshed tabs resurrect a producer-less share

The everyone-refreshed deadlock (bytes survive in IndexedDB, the manifest
didn't, so nobody could serve or vouch and everyone waited on everyone) is
closed: `sync`/`refresh_held` persist the origin's manifest bytes into the
share's `IdbStore` under a reserved `"\0manifest"` slot with a `{size, tree}`
locator in localStorage, and the dead-origin fallback re-arms the waiting
membership from that sidecar — serving and vouching before any peer exists.
The web now has the native mirror's sidecar semantics. Still open: a *single*
re-armed tab serves others but its own UI waits for a peer — rendering the
tree offline from the persisted manifest is the follow-up.

## Browser↔browser data channels stall on bulk (probed around, not fixed)

A tab↔tab WebRTC mount passes JSEP and a manifest and then freezes on bulk
READ responses — a zip download sat at its 39-byte local header on a healthy
`paths webrtc` connection, reproduced twice on a pristine mesh, while other
channels between the same builds carried 600 KB fine. Intermittent per
channel, so the client runs a **bulk probe** at connect (`probe_read`): the
largest live file, up to one MAX_READ_LEN window, failed only when a full
10 s passes with zero bytes — and only on data-channel connections, since
relay and ip never stalled. A channel that stalls is closed and the seeder
is redialled over the relay.

Correction to earlier revisions of this entry: there is **no upgrade
watcher to re-enable** — none was ever built (the wasm client's module doc
explains why in-place upgrade is impossible; an upgrade is a redial). A
relay-carried mount reaches the channel again only on a natural reconnect.
The real defect is in the browser↔browser path of
`fofoca-iroh-webrtc-transport` (bulk was only ever measured browser↔native);
fix it there, then drop the probe's demotion arm — and if relay wins should
become temporary, build upgrade-by-redial (next entry).

## A relay win is permanent: upgrade-by-redial is not built

A mount that settles on the relay keeps it for the connection's whole life,
even when a data channel would carry 4–19 MB/s (docs/perf) — the webrtc-first
dial only runs again when the connection dies and the natural reconnect
rebuilds everything. Nothing is broken, but a browser↔native mount that hit
one transient ICE failure at connect pays relay throughput for hours.

In-place upgrade is impossible (the wasm client's module doc explains: iroh
stops fanning out once the remote selects a path), so the shape is redial and
swap: while `data_path == "relay"`, periodically re-run the webrtc dial; when
a channel forms, bulk-probe it (`probe_read` — the demotion arm in reverse),
and only then swap the client onto the fresh connection. Scope it by pairing:
a relay win against a native peer is safe to retire, a browser↔browser one is
exactly the stall the probe guards. Reuse the waiting membership's hubs
(`WAITING_MESHES`) — a fresh identity per redial would drop every channel the
membership holds and mint the ghost roster entries the overnight-CPU entry
already tracks. The swap is the risky half: reads in flight hold the old
`Connection`, so the exchange needs a seam where nothing is mid-stream.

## ICE restart needs transport renegotiation (fofoca-side)

The wasm client now watches a webrtc mount's `connectionState` through
`BrowserHubTransport::peer_connection` and closes the mount on `failed` /
`closed` at once, or after a 10 s grace on `disconnected` — recovery starts
in seconds instead of waiting out QUIC's idle timeout behind a
connected-looking tab. But the *cheap* recovery is still unreachable:
`restartIce()` re-gathers candidates in about a second where the rebuild
pays a full teardown, JSEP round and candidate race — and calling it is
useless without a renegotiation lane, because the transport's JSEP is one
envelope each way per session (`negotiate` builds a new session; nothing
carries a second offer on an existing one). Fixing that is fofoca work in
`fofoca-iroh-webrtc-transport`: accept a re-offer for an existing session
(`iceRestart: true`), or expose a renegotiation hook the client can drive.
While there, expose `connectionstatechange` as an event instead of the 1 s
property poll the client runs today.

## Fixed since: producer-less browser swarms are true P2P

The dead-origin fallback now builds the full WebRTC shape: the waiting
membership serves MOUNT **and SIGNAL** on its mesh Router, and the mount
lane runs under **its own key** — a seeder's session refusal is keyed to the
TLS-proven id of the signal connection, so offers from the mesh identity
were refused for the mesh lane's session every time; a distinct mount
identity (own relay-bearing signal endpoint + relay-free mount endpoint)
negotiates cleanly and the mount can only settle on the channel. Verified in
Chrome: producer killed, both tabs hard-reloaded, mount `paths webrtc`,
`direct 2/16`, full zip (600,352 B) downloaded tab↔tab.

Provenance: that drill ran against the sibling fofoca checkout at `52a72f76`
— the rev the workspace now pins. It would **fail** at the previous pin
`e63a481f`, which predates the rival-probe, STUN and JSEP-datagram fixes the
scenario exercises (the checkout may also have carried a then-uncommitted
`beacon/mod.rs` edit that is in no pinned rev). The run also had the bulk
probe's demotion arm active (previous section): the probe happened to pass on
webrtc that time, but the stall it guards is intermittent per channel, so
this entry is not evidence for dropping the probe's demotion arm — that
unwinds only with the transport fix.

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

- **The dev server serving stale JS glue against a fresh wasm.** The reverse
  twin of the stale-`.wasm` entry below: the binary healed itself through the
  content-addressed URL, but the glue was bundled straight out of the crate's
  `dist/web/` — outside `web/`, where bun's watcher never looks — so a
  `cargo task web-wasm` mid-session paired new wasm with old glue and died at
  `LinkError: … __wbg_connectionState… function import requires a callable`
  (caught 2026-08-07 while validating the channel-health watcher; the error
  names whichever binding only one side knows about, not the real problem).
  The glue is now mirrored into `src/wasm-glue/` (generated, gitignored) by
  `scripts/wasm-asset.ts:syncGlue`, `src/wasm.ts` imports the mirror, and
  `dev.ts`'s rebuild watcher re-syncs it beside the wasm re-hash — a rebuild
  heals the whole pair without a server restart.

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
