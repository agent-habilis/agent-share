# Testing

Three suites, and they do not overlap:

| suite | what it proves | in the gate |
|---|---|---|
| `cargo task ci` | the code compiles, lints, and its units behave | yes |
| `cargo task e2e` | a real browser and a real binary interoperate over a real network | no |
| `cargo task bench` | how fast each transport is, as numbers | no |

`ci` is the only one that gates a change. `e2e` and `bench` need a browser, a
built wasm and network reach, so they run by hand — and a missing prerequisite
is reported as a **skipped row naming the reason**, never a quiet pass.

## Naming

`cargo task naming` — also the first step of `ci` — checks what files and
folders are called. One rule per language:

- **snake_case** inside a crate, because a Rust module file name *is* the
  module name: `mod mesh_key;` only ever finds `mesh_key.rs`.
- **kebab-case** everywhere else.

A crate is a folder whose `Cargo.toml` has a `[package]` section, so the crate
folder itself keeps its package spelling (`crates/agent-share-proto/`) and only
what lives under it is module naming. `Cargo.toml`, `README.md`, `Dockerfile`
and the few other names the ecosystem picks are allowed as-is.

It reads `git ls-files`, so generated and ignored trees are out of scope, and
it prints a suggested name per offender — a failure is a rename list, not a
complaint. It runs first because it is the only step of `ci` needing no
toolchain and having no skip path.

## The coverage matrix

`cargo task e2e` prints this at the end of every run. Each row records which
implementation produced the share, which one read it, and over what lane.

Three implementations, and they are not interchangeable:

- **native** — the Rust binary. The only one that can mount (loopback NFSv3) and
  the only one that can produce from a real folder.
- **web** — the browser app. Produces through the File System Access picker,
  which needs a user gesture; `/app/lab` stands the same producer up from OPFS
  handles and constructed `File`s so it can be driven headlessly.
- **node** — `npx agent-share <ticket>`. Receive-only, writes real files. It
  cannot produce a folder, by design: scan order is the read index, and that
  stays on the native binary.

### Pairings

| producer \ consumer | native | web | node |
|---|---|---|---|
| **native** | 8/8 | 18/18 | 1/1 |
| **web** | 3/3 | 1/1 | — |
| **node** | — | — | — |

31 of 31 rows passed, none skipped, on macOS 27 with Chrome 152, `agent-browse`
0.5.0 and a working relay — and passed identically on **three consecutive full
runs**, 93 of 93 row-executions, with the three matrices byte-identical. The
count in each square is `passed/selected`; a square that reads `—` is a pairing
no row covers, and the gaps are listed at the end of this file.

### Rows

| cell | producer | consumer | transport | verdict |
|---|---|---|---|---|
| `web-list` | native | web | dynamic | pass |
| `web-download-single` | native | web | dynamic | pass |
| `web-download-zip` | native | web | dynamic | pass |
| `web-download-dismissed` | native | web | dynamic | pass |
| `web-reconnect` | native | web | dynamic | pass |
| `web-producer-gone` | native | web | dynamic | pass |
| `web-live-update` | native | web | dynamic | pass |
| `web-live-delete` | native | web | dynamic | pass |
| `web-seeder-propagation` | native | web | dynamic | pass |
| `web-seeder-propagation-two-tabs` | web | web | dynamic | pass |
| `web-transport-webrtc` | native | web | webrtc | pass |
| `web-transport-relay` | native | web | relay | pass |
| `web-password` | native | web | dynamic | pass |
| `web-password-no-producer` | native | web | none | pass |
| `password-native-live-right` | native | native | quic | pass |
| `password-native-live-wrong` | native | native | none | pass |
| `password-native-dead-wrong` | native | native | none | pass |
| `password-native-dead-right` | native | native | quic | pass |
| `password-native-absent` | native | native | none | pass |
| `password-native-spurious` | native | native | none | pass |
| `password-native-seed-reserve` | native | native | quic | pass |
| `password-legacy-ticket` | native | native | quic | pass |
| `password-web-dead-right` | native | web | none | pass |
| `password-web-persist` | native | web | dynamic | pass |
| `password-node-cli` | native | node | relay | pass |
| `password-web-producer` | web | native | relay | pass |
| `password-web-producer-snapshot` | web | native | relay | pass |
| `web-producer-webrtc` | web | native | webrtc | pass |
| `webmcp-read` | native | web | dynamic | pass |
| `webmcp-failures` | native | web | dynamic | pass |
| `webmcp-ui` | native | web | dynamic | pass |

## Which lane covers what, and why

**`seed --copy-only`, not the mount form.** Every native-consumer row shells out
to `agent-share seed --copy-only`. It needs no NFS, no mountpoint and no privileges, it exits
on its own, and it goes through the same `redeem_auth` gate every consumer path
does. Testing the mount would test the OS's NFS client.

**A second Chrome profile for the two-tab row.** `agent-browse` keys a profile by
the window's folder, so `web-seeder-propagation-two-tabs` opens its second tab
against a different directory. The separate profile is the point, not a side
effect: one shared IndexedDB would let the "fresh" consumer serve itself from its
own chunk store and pass a propagation test while proving nothing.

**`--transport webrtc` on a native consumer.** Not a preference — a requirement.
It strips the alternatives rather than preferring the channel, and asserts the
selected path afterwards, so a row that passes under it has demonstrably used
the data channel. It exists to test the browser lane from a native process, and
to let the bench harness measure it. It is not a transport to choose: between two
native peers the data channel costs 6× throughput and 36× latency against plain
QUIC.

**Why the `none` lane is a result.** The `password-native-dead-*`, `-absent` and
`-spurious` rows dial nothing at all. A wrong password on a protected share is
ruled locally, against the verifier in the ticket's mesh id, and these rows assert
that it is refused in under five seconds. A lane would mean the ruling had become
a network wait — so `none` is the assertion, not missing information.

**Why a browser row cannot run offline.** A browser reaches a native producer by
brokering signalling over the iroh relay before the direct data channel opens.
There is no loopback path for it, which is why `missing_prerequisite` gates the
whole suite on network reach rather than gating individual rows.

## Prerequisites

Checked once, before any row runs. Any of them missing skips **every** selected
row with that reason and exits 0 — a harness that is not set up is not a red
suite.

| needs | gates | install |
|---|---|---|
| `agent-browse` | everything | see the agent-browse repo |
| the built wasm | everything | `cargo task web-wasm` |
| network reach | everything | — |
| Chrome 150+ | `webmcp-*` | `agent-browse chrome install --execute` |
| `node-datachannel` | `password-node-cli` | `cd packages/agent-share-node && npm install` |

Two things deliberately **fail** rather than skip, because a precheck cannot see
them: a Chrome without OPFS on `password-web-producer`, and a Chrome new enough
to pass the version gate that publishes no `document.modelContext`. Both would
otherwise pass by not looking.

## Running it

```
cargo task e2e                      # all rows, ~30 min
cargo task e2e --cells web-list     # one row
cargo task e2e --cells password     # every password-* row
```

`--cells` matches exact names first, then treats a name as a group prefix. Note
`--cells web` also selects `webmcp-*`, since the match is a plain prefix; `web-`
does what you meant. An unknown name is an error listing every known row.

The matrix goes to **stdout** and the per-row status lines to **stderr**, so
`cargo task e2e > matrix.md` captures the table on its own.

Runtime is dominated by the browser rows: each pays a fresh window launch, a
cache clear, a reload and roughly ten seconds of relay-brokered signalling. There
is no parallelism knob, and a bad network inflates the total badly.

## Flakiness

Two flakes were found by running the suites repeatedly rather than once, and
both are fixed. Recording them because the same shapes will recur.

**`Network.clearBrowserCache` timing out.** Measured over three full runs it
failed 4, then 1, then 2 rows — always that one CDP call, never anything after
it. It is now best-effort: retried once, then warned past. Losing it costs
nothing, because the binary is served from a content-addressed URL, so a stale
cache entry is unreachable by construction rather than by clearing. See
`clear_cache` in `tasks/src/bench/browser.rs`.

**CDP issued before the window was drivable.** `launch` returning does not mean
Chrome will answer. The harness already knew this for `evaluate` but sent the
cache-clear and reload ahead of the first readiness wait. `await_window` now
gates every window before anything drives it.

**An interrupted run poisons the next one.** Killing a run mid-flight leaves
Chrome and `agent-browse` processes alive — 35 and 27 were observed once — and
every later run then fails its browser rows on the timeout above until they are
killed by hand. `reap::reap_stale` tries, but it cleans up by asking
`agent-browse quit`, which needs the CDP channel that is wedged. Let a run
finish, or kill the leftovers yourself.

**A unit test, not an e2e row:** `mount::mesh`'s four tests that bind real
endpoints now hold a mutex so only one mesh is alive at a time. The three-engine
row missed its deadline about once in five `cargo test --workspace` runs;
raising the deadline did not help (the failing run burned the whole 120 s), so
the cause was starvation, not slowness. Serialized, it passed 8 runs for 8.

## Known gaps

Stated rather than left blank, because a blank square in a coverage table reads
as a zero someone already thought about.

- **node → web** and **node → native bench** are untested. The node receiver has
  a synthetic `BenchProducer` that nothing drives.
- **web → web** is covered only by the seeder-propagation row, which starts from
  a native origin. No row has a tab produce for a tab from scratch.
- **node has no row of its own beyond `password-node-cli`**, which now dials
  the data channel like every other consumer.
- **node cannot produce a folder** at all — see above. That square is not a gap
  but an absence by design.
- **Safari and Firefox are never exercised.** `password-web-producer-snapshot`
  runs the `File`-snapshot arm those browsers would take, but it runs it in
  headless Chrome, so it proves the branch and not the browser.
