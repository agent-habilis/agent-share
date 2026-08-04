# agent-habilis-mesh (vendored)

The serverless gossip-network engine, and the engine `agent-share` runs on —
both on the CLI and, through a wasm build, in the browser.

## Provenance

Copied from `agent-habilis/agent-gossip`, `crates/agent-habilis-mesh`, at
upstream commit **`8914557`**, and modified here.

This is a deliberate fork, not a snapshot to leave alone: the wasm work below
does not exist upstream. **Generalising it back out — to agent-gossip, or to a
crate both products depend on — is a scheduled follow-up**, not a loose end.
Until then, changes here and upstream will drift; re-syncing is a diff against
`8914557`.

The workspace `[workspace.dependencies]` block in the repo root is kept
byte-compatible with the manifest this came from, so that diff stays readable.

## What changed: a wasm32 target

Upstream is host-only. The browser needs the same engine, so everything that
requires an operating system now sits behind a **`host` feature, on by
default**. `--no-default-features` reaches `wasm32-unknown-unknown`.

Gated out for wasm:

| | why |
|---|---|
| `transport::ipc` | `interprocess` — unix sockets / named pipes |
| `daemon::{event_loop, state_file, config, node, setup, app, timers, params}` | signals, processes, the session file — the daemon *shell* |
| `logging::sink` | writes a log file |
| `util::{process, resident_memory}` + the `/tmp` runtime-dir helpers | `libc`, filesystem ownership checks |
| `blob::{produce, store}`, `ops::blob::{BlobServer, offload}` | reads files from disk |
| `lookup::{mdns, dht, capability}` | UDP sockets; net-report is a host-only iroh feature |
| `iroh-multihop-transport` | forwards real UDP packets |

Kept — and this is the point — `gossip`, `doc` (the automerge CRDT channels),
`protocol`, `identity`, `reassembly`, `resolver`, `invite`, `beacon`,
`directory`, `lookup`, blob *consumption*, and `daemon::{state, ctx,
message_log}`. `iroh-gossip` with its `net` feature does compile for wasm, so
the browser runs the real gossip layer rather than a reduced stand-in.

## What changed: a WebRTC transport

`build_endpoint` used to take `multihop: Option<MultihopHandle>` as a
positional argument. It now takes a `TransportHandles` struct, because the set
of custom transports grows and several are host-only — so the *field list
itself* differs per target:

```rust
pub struct TransportHandles {
    #[cfg(feature = "host")]
    pub multihop: Option<iroh_multihop_transport::MultihopHandle>,
    pub webrtc: Option<fofoca_iroh_webrtc_transport::WebRtcHandle>,
}
```

`webrtc` is not optional: it is the browser's only way onto the mesh. Only its
backend differs by target (str0m natively, `RTCPeerConnection` in a tab), and
`WebRtcHandle` is the same type either way, so the wiring is written once.

Registration is additive (`add_custom_transport`) rather than a `Preset`. A
preset would make WebRTC the endpoint's *only* transport — right for a browser,
wrong for a native peer that should still prefer iroh's hole-punched paths.

### Sessions are negotiated only with peers that need them

Upstream, `negotiate_session` runs for every peer it sees, gated only on id
order and the direct-peer cap. Here it also requires the peer to advertise **no
IP transport** — see `needs_webrtc_lane` in `src/transport/webrtc.rs`.

A peer reachable over IP is reachable over plain iroh QUIC, and in `agent-share`
that is measured at 6× the throughput and 1/36th the latency of the data channel
(`docs/perf/` in the parent repo). Negotiating a channel between two native
peers spent a JSEP round trip and a DTLS stack to end up with a worse path
sharing the endpoint and congestion domain with file bytes. A browser has no IP
stack under wasm and so advertises relay-only, which is exactly the condition
this tests.

This is one of the changes worth pushing back upstream: nothing about it is
`agent-share`-specific.

## Building

```bash
cargo check -p agent-habilis-mesh                       # host
cargo check -p agent-habilis-mesh --no-default-features # portable, native
CC=/opt/homebrew/opt/llvm/bin/clang \
CC_wasm32_unknown_unknown=/opt/homebrew/opt/llvm/bin/clang \
cargo check -p agent-habilis-mesh --no-default-features --target wasm32-unknown-unknown
```

`ring`'s C core cannot be built for wasm32 by Apple clang. `getrandom` needs
both a feature and the `getrandom_backend="wasm_js"` rustflag — the flag lives
in the repo's `.cargo/config.toml`, because the feature alone is not enough and
the failure message does not say so clearly.

`cargo task ci` runs the wasm check, so the browser target fails here rather
than rotting until someone tries to build the client.
