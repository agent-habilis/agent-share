# What is tested, what is not, and who runs it

The dimensions of this product — peer pair × transport × share shape ×
connection lifecycle × action — cross to thousands of combinations. Nobody runs
thousands of combinations, and a matrix nobody runs is worse than none, because
it reads like coverage.

So this document does not enumerate a grid. **Every cell below earns its place
by naming something that broke, or an invariant whose violation would be
silent.** Cells with neither are not listed.

Four lanes. Every cell states which one it is in.

| lane | who runs it | when |
|---|---|---|
| **A — gate** | `cargo task ci` | every change |
| **B — e2e** | `cargo task e2e` | on demand; needs a browser and a built wasm |
| **C — manual** | a person | before a release, or when touching what it covers |
| **D — uncovered** | nobody | stated so it is a decision, not an oversight |

## The dimensions

| dimension | values |
|---|---|
| peer pair | native ↔ native · native ↔ web · web ↔ web |
| transport | iroh QUIC · iroh relay · `WebRTC` · dynamic (prefer `WebRTC`, fall back to relay) |
| runtime | native CLI · Chrome · Safari |
| share shape | one large file · many small files (>100) · nested dirs · empty |
| connection lifecycle | fresh · idle · killed · producer gone |
| action | list · download one · download many (zip) · mount · live update |

Two rules that shrink it honestly. **Transport is only interesting where it can
differ** — native ↔ native is pinned to iroh transports
(`crates/agent-share/src/mount/webrtc.rs`), so a `WebRTC` cell there tests the
*override*, not the default. And **share shape only matters where a count or a
size crosses a limit**: >100 files against the producer's 100-stream ceiling,
and one file large enough to need many reads.

---

## Lane A — the gate

Runs in `cargo task ci`. Strong where it exists, and it exists almost entirely
below the transport.

| area | where | what it holds |
|---|---|---|
| wire format | `agent-share-proto` golden pins (`wire_constants_are_pinned`, `type_bytes_are_pinned_wire_format`, `flag_bytes_are_pinned_wire_format`), `swarm_id_wire_format_is_pinned` | a byte change breaks every issued ticket and every older peer, so it must be a deliberate edit |
| message format | 6 `insta` snapshots + 16 `prop_*` proptests in `agent-habilis-mesh` | round-trips, hostile input, size limits |
| transport selection | `crates/agent-share/tests/transport_modes.rs` (4) | including `native_to_native_selects_ip_even_with_a_live_webrtc_session` — a live data channel must still lose to IP |
| the `WebRTC` lane | `crates/agent-share/tests/webrtc_mount.rs` (4), `fofoca-iroh-webrtc-transport/tests/loopback.rs` (3) | JSEP, session registry, path selection, echo over the channel |
| multihop | `iroh-multihop-transport/tests/e2e.rs` (2) | selected path is the custom transport |
| wasm primitives | `agent-habilis-mesh/tests/wasm_runtime.rs` (5) | clock, timers, `spawn`, `getrandom` — the things that compile and then panic |
| the browser client | `agent-share-wasm-client --lib` (15), run **on wasm32** | transport-mode parsing and the live-tree state machine |
| the web app | `bun test` (404) + `tsc --noEmit` across five projects | the app's own helpers, and that the TypeScript still type-checks |

The browser client's tests run on wasm32 rather than the host, and that is
forced rather than chosen: off wasm32 `agent-habilis-mesh` enables
`fofoca-iroh-webrtc-transport/host`, and with both backends on `WebRtcHandle`
resolves to the host one while the client hands it a `BrowserHubTransport`. A
host build of that crate cannot type-check by construction. The gate used to
run it on the host anyway, and had been red for it.

**Known gate weaknesses**, recorded rather than fixed here:

- `the_mount_selects_webrtc_over_a_warm_relay_path` needs a real relay and is
  not `#[ignore]`d, so an offline run is a red build for a reason unrelated to
  the change.
- Without a wasm-capable clang the whole wasm leg **skips green**, including
  `wasm_runtime` and the browser client's 15 tests.
- Without `bun`, the whole web leg skips green.
- Of the 404 `bun test` cases, ~84% belong to the *vendored* UI framework. Only
  about 62 are app code, and all of those are pure helpers — nothing renders
  `App.tsx` or drives a session. That gap is what lane B exists for.

---

## Lane B — `cargo task e2e`

Headless Chrome against a real producer, one window and one `agent-share serve`
per cell. Not in the gate: it needs `agent-browse`, a built wasm, and the
network — a browser reaches a native producer by brokering signalling over the
relay before any direct path exists, so *every* cell needs one, not just the
relay cell. A missing prerequisite produces a **skipped row per cell with a
reason**, never a silent pass.

`cargo task e2e --cells web-reconnect,web-download-zip` runs a subset; an
unknown name is an error, because a typo that selects nothing would otherwise
report a clean pass.

| cell | asserts | why it exists |
|---|---|---|
| `web-list` | the manifest arrives and every file appears in the tree | the floor: nothing below matters if this breaks |
| `web-download-single` | bytes are **identical to source**, by digest | nothing anywhere compares transferred content; the bench checks a byte *count* against `io::sink()` |
| `web-download-zip` | a 301-file share arrives as an archive holding 301 entries | a large multi-file archive completing at all — see the caveat below |
| `web-download-dismissed` | dismissing the save dialog leaves **nothing** on the page, and the next download still works | closing the dialog printed `Failed to execute 'showSaveFilePicker'…` in red; the cancel suppression tested the Cancel *button*, which a dismissal never presses |
| `web-reconnect` | kill the connection → it revives unprompted, and the revived session **delivers the file** | a backgrounded tab lost its connection and every action failed until reload — a page that merely *looks* connected is the bug, so the cell downloads |
| `web-producer-gone` | the failure appears on the page | it appeared as an unhandled rejection in a crash overlay, naming an operation that was not at fault |
| `web-transport-webrtc` | bytes flow with `?transport=webrtc` | the lane browsers depend on |
| `web-transport-relay` | bytes flow with `?transport=relay` | the fallback when ICE fails — which is what a real Safari session did |

**Two invariants checked on every cell, not just their own:**

1. **No unhandled rejections.** A listener is installed before the first action
   and read at teardown. Free to check everywhere, and precisely the class that
   produced the crash overlay.
2. **The served wasm matches disk**, before any cell runs. Twice in one session
   a mismatched build masqueraded as a product bug — once as
   `CompileError: … Custom section … would overflow Module's size`, once as
   `decode ticket: ticket address truncated`, a string present in neither the
   source nor the binary. A harness that silently tests the wrong build is
   worse than no harness.

### Every cell was made to fail on purpose

A cell that cannot fail is not a test, so each was checked against a deliberate
regression rather than assumed to work:

| cell | injected fault | result |
|---|---|---|
| `web-download-single` | flip one byte in `fileStream`'s first chunk | **fails** — digest mismatch |
| `web-reconnect` | disable the 1 s liveness poll | **fails** — "timed out waiting for the app to notice the connection died", and the page dump shows a healthy-looking share with an enabled Download button, which is exactly the bug's signature |
| every cell | `Promise.reject` inside `bringUp` | **fails** — the rejection invariant catches it |
| `web-list`, `web-transport-*`, `web-producer-gone` | dev server killed mid-run | **fail** — `ERR_CONNECTION_REFUSED`, quoted from the page |
| `web-download-dismissed` | narrow the suppression back to `abort.signal.aborted` alone | **fails** — the DOM message is quoted back out of the page dump |
| `web-download-zip` | build the zip entries eagerly with `.map()` | **passes — the cell does not catch it** |

That last row is a real limit and is recorded rather than papered over. Reverting
`zipStream` to eager entries passed at 301 files and again at 1201 — quinn
*queues* stream opens past `max_concurrent_bidi_streams` instead of failing, so
the archive still completes. The lazy-entries change (`f0c9032d`) is therefore
hardening rather than a fix for a failure this cell can observe. What the cell
does guard is the outcome: a large multi-file archive arrives whole, with the
right entry count.

### How the byte comparison is made

`showSaveFilePicker` is replaced with a memory sink that digests what it is
handed, so the cells read a SHA-256 (and, for the archive, its entry count)
instead of chasing a file into Chrome's download directory. The write to disk is
the browser's; everything upstream of it — chunked reads, the zipper, `pipeTo`,
the abort wiring — is still exercised.

The stub also stands in for a user who says *no*: with `window.__e2eSaveAbort`
set it rejects with the `AbortError` a dismissed dialog produces, and it counts
its calls in `window.__e2eSaveAsks` so a cell can tell a dismissed dialog from
one that never opened — both leave nothing saved.

What no cell can assert is that the progress bar stays down *while* the dialog
is open: the stub settles in a microtask, so there is no window to observe. That
half of the fix — the picker opens before the transfer starts, so the dialog no
longer sits over a `downloading 0%` row — is a manual check.

### The dev server takes an ephemeral port

`PORT=0`, not `dev.ts`'s default 3000. The default collided with a **sibling
worktree's** dev server that re-took the port within seconds of being freed, so
the harness alternated between `EADDRINUSE` and something worse: adopting a
server that was serving another checkout's build, where the wasm lives under a
content-addressed name and the app never starts. Four cells failed that way in
one run and passed individually — the exact shape of a harness bug that reads as
a product bug.

---

## Lane C — manual

Things no `cargo task` can drive. Each says what it covers that lane B cannot.

### Safari

**The lane that found the reconnect bug.** Chrome failed to reproduce it across
three attempts. Not automatable here: `safaridriver` needs a one-time
privileged enable and is documented in-repo as unreliable on this host.

| check | steps | expected |
|---|---|---|
| idle reconnect | open a share, leave the tab **hidden** 5+ min, return, press Download | reconnects and downloads; no reload needed |
| no File System Access | press Download | the in-memory Blob path is used; there is no save dialog — Safari has no `showSaveFilePicker` |
| dismissed save dialog (Chrome) | press Download, close the dialog | nothing changes: no progress bar goes up while the dialog is open, and no red text is left behind |
| timer throttling | in a hidden tab, time `setTimeout(1000)` | gaps stretch to 13–20 s after ~10 s hidden; this is what starves QUIC's keep-alive |
| relay fallback | open with a producer ICE cannot reach | Info shows `transport relay` and a `fell back:` reason, and bytes still arrive |

### The two `#[ignore]`d tests

Deliberately left ignored — both fail for environmental reasons, and a gate
that fails environmentally is a gate people learn to skip.

| test | run it | covers |
|---|---|---|
| `real_mount_round_trip` | `cargo test --test mount -- --ignored` | the **only** test that mounts through the OS NFS client, reads bytes, and proves the mount is read-only |
| `the_real_cli_serves_over_webrtc` | `cargo test --test e2e_cli_webrtc -- --ignored` | the **only** test driving the shipped binary over a data channel, including a 300 KB blob across many chunks |

### Deploy cache

After changing the wasm, load the built site as a **returning visitor** (a
browser holding the previous copy) and confirm it still starts. `build.ts`
content-hashes the wasm so a stale copy is unreachable — this check is what
would notice that regressing. Note a browser that cached before `no-store`
existed keeps serving it; `fetch(url, { cache: 'reload' })` replaces the entry
without emptying the cache by hand.

---

## Lane D — uncovered, on purpose or otherwise

| area | note |
|---|---|
| `tasks/` | zero tests, and `bench/{proc,reap}.rs` manage subprocesses and mount cleanup |
| `node/` npx CLI | no tests, no typecheck in the gate; its `node-datachannel` addon does not build under a bun install |
| cross-version | golden pins stop the format drifting, but nothing decodes a frame produced by an **older build** |
| degraded network | no coverage of offline, relay-down or NAT-blocked paths |
| web ↔ web | two browsers sharing to each other; `/lab`'s mesh panel is the only way to exercise it, by hand |
| browser mount (OPFS) | `web/src/mount.ts` has no automated coverage |
| a browser **producing** real files | needs the File System Access picker, which needs a user gesture — structurally not automatable |

---

## Choosing what to run

- Changed the wire format, framing or a protocol type → **lane A** is the one
  that matters, and a golden pin failing means you broke compatibility, not
  that the test is stale.
- Changed the web app, the wasm client or the transport → **lane B**, then the
  Safari checks in lane C, because Chrome and Safari have disagreed before.
- Changed mount, NFS or the producer's read path → **lane A** plus
  `real_mount_round_trip` from lane C.
- Preparing a release → all four, and read lane D so you know what you are
  shipping blind.
