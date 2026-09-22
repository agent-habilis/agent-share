# agent-share

`agent-share` — share a folder with peers, or mount a peer's folder locally.
Read-only, lazy, no daemon: the producer serves file bytes on demand over an
[iroh](https://github.com/n0-computer/iroh) QUIC connection, and the consumer
mounts them through a loopback `NFSv3` bridge — the OS's built-in NFS client,
no FUSE, no kernel extension.

Extracted from [agent-habilis/swarm](https://github.com/agent-habilis/swarm)'s
`ahsw mount`, and since forked: `agent-share` negotiates its own QUIC ALPN, so
it no longer mounts against `ahsw mount` in either direction. Both ends must
run `agent-share`. The ticket *framing* (Base58Check over `version ‖ type ‖
payload`) is still shared with the rest of the agent-habilis tooling, but
agent-share's tokens carry no glyph prefix — they are bare ASCII Base58.

## Install

```sh
# Homebrew for macOS and Linux
brew install agent-habilis/tap/agent-share

# From source everywhere else
cargo install --git https://github.com/agent-habilis/agent-share agent-share
```

Prebuilt binaries exist for macOS (Apple silicon and Intel) and Linux (x86-64
and ARM64). The Linux binaries need glibc 2.39 or later (Ubuntu 24.04, Debian
13).

## Usage

Share a folder (producer):

```
agent-share serve <dir>
```

This prints the consumer's ready-to-run command, carrying a bearer
ticket. The scan is metadata-only; peers fetch file bytes on demand as they
read them. The folder is watched, so edits, new files and deletions reach
connected peers without anyone remounting. Serve keeps running until
interrupted.

A file's position in the manifest is the address reads use, so those positions
are append-only for the life of a `serve`: a deleted file leaves its slot
behind rather than renumbering everything after it. Reusing slots would let a
peer holding an older listing read one file's bytes under another file's name,
with no error to notice — the same reason mounted filehandles keep their ids
across an update.

Mount it (consumer):

```
agent-share <ticket> <target>
```

Creates `agent-share-YYYY-MM-DDTHHMM/` under `<target>` (which may already have
other files), mounts there, and unmounts on Ctrl-C. Writes fail — the mount is
read-only.

### Discovery

By default a share is reachable across machines (mDNS + mainline DHT + the
default relay ladder). To restrict it, either name lookup flags explicitly
(`--mdns`, `--dht`, `--relay [<url>,…]` — naming any uses only those) or pass
`--swarm <id>` to reuse an existing swarm id's discovery config (a loopback
swarm id keeps everything on one host).

## Platform support

macOS (`mount_nfs`) and Linux (`mount -t nfs`, which usually needs sudo — the
mount command is printed so you can run it yourself if the automatic step
fails). Other platforms can still `serve`.

## Development

```
cargo build
cargo task fmt
cargo clippy --all-targets
cargo test                          # includes a subprocess bridge test, no OS mount
cargo test --test mount -- --ignored  # the real OS-mount round trip, run by hand
```

Imports form one block at the top of the file, before any `mod`, in three
groups: std, then external crates (the workspace crates included), then
`self`/`super`/`crate`. Write a child-module import as `self::child::…` so that
it sorts into the last group. `cargo task fmt` runs nightly rustfmt (pinned in
`tasks/src/fmt.rs`) for the `group_imports` option in `rustfmt.toml`, and
`cargo task ci` fails on drift. rustfmt does not move a `use` that comes after
a `mod`.

The wire format is pinned by golden tests (`wire_constants_are_pinned`,
`type_bytes_are_pinned_wire_format`, `swarm_id_wire_format_is_pinned`) — if one
of those fails after a change, you broke compatibility with already-issued
tickets and with peers running an older build.

### Releasing

```
cargo task release minor              # dry run
cargo task release minor --execute    # bump, commit, annotated tag — no push
git push origin main --follow-tags
```

The tag push starts `.github/workflows/release.yml`. It builds the four
binaries, creates the GitHub release, and updates `Formula/agent-share.rb` on
`main`. Then it copies the formula to `agent-habilis/homebrew-tap`. That copy
needs the `TAP_PUSH_TOKEN` Actions secret: a fine-grained PAT with contents
read/write on the tap repo.

## The web client

`agent-share.dev` is a **pure static site**: no backend, no signalling
server, no database. The root is the landing page, `/docs` the docs, and
`/app` the web client. Open `agent-share.dev/app/files/<ticket>` and the
browser connects straight to the producer. Session info is at
`/app/info/<ticket>`.

The ticket is a bearer capability in the path so those views are ordinary
shareable URLs. The static host must fall back to `app/index.html` for deep
links under `/app`.

### Driving it with an agent

The page publishes its own actions — open a share, list it, read a file, seed
it, publish one — as [WebMCP](https://webmachinelearning.github.io/webmcp/)
tools, so an agent can use the browser as its runtime and install nothing. Needs
Chrome 150+ and `chrome-devtools-mcp`; see
[the WebMCP docs](packages/agent-share-site/content/docs/webmcp.mdx).

### How a browser reaches a peer behind NAT

Two connections, and the split is load-bearing:

1. The browser dials `agent-share/webrtc-signal/1` **over the iroh relay** and
   swaps one JSEP envelope each way.
2. It then opens a **fresh** connection to `agent-share/mount/1` against an
   address carrying only the WebRTC custom addr.

It has to be two, because iroh only fans a connect's Initial out to candidate
paths while the remote has no selected path — a live connection cannot be
upgraded onto a newly attached transport.

For that handshake the relay is a rendezvous rather than a transport: it carries
the SDP exchange, not file data.

### Which transport carries bytes

Direction matters on the mixed pair, so the rows name a producer and a consumer
rather than a symmetric "↔".

| producer → consumer | carries bytes | never |
|---|---|---|
| native → native | iroh QUIC, else iroh relay | **WebRTC** |
| native → web | WebRTC, else iroh relay | — |
| web → native | iroh relay, unless forced onto WebRTC | — |
| web → web | WebRTC, else iroh relay | — |
| native → node | iroh relay | — |

A browser consuming a native producer prefers the data channel. The reverse is
not its mirror image: a tab is publicly reachable or not reachable at all — no
mDNS, no DHT, no loopback peers — so its ticket advertises a relay URL, and a
native consumer dials that unless `--transport webrtc` takes the alternatives
away. The node receiver pins the relay outright.

WebRTC exists because a browser has no UDP socket and cannot speak QUIC
directly. That is the whole of its justification, so it never carries bytes
between two native peers: measured with the transport as the only variable, the
data channel gives **6× less throughput at 36× the latency** and an order of
magnitude more run-to-run variance than plain QUIC.

The two ends behave differently when the preferred path fails, and both are
deliberate. A **browser** falls back to the iroh relay when ICE fails, so it
degrades rather than dying — at relay speed, but it connects. A **native** peer
does not fall back onto the data channel: if it can reach the producer over
neither IP nor relay, the mount fails. The pair that costs is one ICE could have
joined while hole-punching and the relay both failed, which is narrow, since
losing the relay usually means losing the network.

`--transport webrtc` forces a native consumer onto the lane anyway. It exists to
test the browser path from a native process and to let the benchmark harness
measure it — not as a transport to choose.

### Building

```
bun install && bun run build
```

`packages/` is the JavaScript half — a Bun workspace beside the `crates/` cargo
one, with the same flat shape. `agent-share-web` is the whole browser half in one
package, over `agent-share-wasm`: the share logic under `src/lib/`, the UI kit
under `src/components/`, and the routes, service worker and lab that bundle them.
The logic is not a package of its own because almost none of it could be shared
anyway — it reaches for `showDirectoryPicker`, `navigator.serviceWorker` and the
OPFS. The exception is the stream protocol, which `scripts/serve.ts` imports by
path so the static host's 404 rule cannot drift from the service worker's URL
prefix. `agent-share-node` is the
`npx agent-share <ticket>` receiver; its directory name carries the
`agent-share-` prefix its npm name (`agent-share`) does not. The six `visage-*`
and `moonspace-*` members are vendored — see `docs/vendoring.md`.

Browser and receiver consume the same `.wasm`; only the wasm-bindgen glue
differs, and `scripts/build-wasm.ts` writes both layers into
`agent-share-wasm` — the browser's ES-module glue into `src/glue/`, the
receiver's CommonJS glue into `node/` beside the binary it reads at import time.
So `agent-share-node` imports `agent-share-wasm/node` by name rather than
reaching into a build directory, and those two are the members that get
published: the receiver, and the wasm package it depends on.

`bun run build` builds that wasm too, through `scripts/build-wasm.ts` — so
it needs the `wasm32-unknown-unknown` target, the `wasm-bindgen` CLI, and a
clang that can emit wasm32 (`brew install llvm` on macOS; Apple's has no wasm
backend). It says which one is missing. `cargo task web-wasm` runs the same
script when only the binary is wanted.

`bun run dev` serves at `https://agent-share.localhost` — a name instead of a
contended port, via [portless](https://github.com/vercel-labs/portless) — and
expects the wasm to exist already. It builds the landing page and docs
(`packages/agent-share-site`, Next + Nextra) once, then serves them around the
app at `/app`, which hot-reloads. For live docs editing, run `bun run dev` in
`packages/agent-share-site`. `bun run build && bun run start` is the
production pair: `start` serves the built `dist/` on `PORT`, with the same
routing the deployed image uses (`scripts/serve.ts`).

### Deploying

```
bun run dev:docker           # build the image and run it here, on :3000
cargo task web-image         # build linux/arm64, push to the Gitea registry
```

The image is Bun serving `dist/`, and it rebuilds the wasm from source rather
than copying a local `dist/`, so it cannot ship a stale binary. `deploy/compose.yaml`
is what runs on the host — read its header first: the app needs a secure context
(HTTPS or localhost) or media previews silently lose the ability to seek.

Note `npx` needs a native WebRTC addon (`node-datachannel`), because Node has
no `RTCPeerConnection` and the relay will not carry data. The native binary
needs no addon and can mount the share as a filesystem.
