# agent-share

`agent-share` — share a folder with peers, or mount a peer's folder locally.
Read-only, lazy, no daemon: the producer serves file bytes on demand over an
[iroh](https://github.com/n0-computer/iroh) QUIC connection, and the consumer
mounts them through a loopback `NFSv3` bridge — the OS's built-in NFS client,
no FUSE, no kernel extension.

Extracted from [agent-habilis/swarm](https://github.com/agent-habilis/swarm)'s
`ahsw mount`, and since forked: `agent-share` negotiates its own QUIC ALPN, so
it no longer mounts against `ahsw mount` in either direction. Both ends must
run `agent-share`. The `🐝…` ticket encoding itself is still shared with the
rest of the agent-habilis tooling.

## Usage

Share a folder (producer):

```
agent-share serve <dir>
```

This prints the consumer's ready-to-run command, carrying a `🐝…` bearer
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
agent-share <🐝…> <target>
```

Creates `agent-share-YYYY-MM-DDTHHMM/` under `<target>` (which may already have
other files), mounts there, and unmounts on Ctrl-C. Writes fail — the mount is
read-only.

### Discovery

By default a share is reachable across machines (mDNS + mainline DHT + the
default relay ladder). To restrict it, either name lookup flags explicitly
(`--mdns`, `--dht`, `--relay [<url>,…]` — naming any uses only those) or pass
`--swarm <🐝…>` to reuse an existing swarm id's discovery config (a loopback
swarm id keeps everything on one host).

## Platform support

macOS (`mount_nfs`) and Linux (`mount -t nfs`, which usually needs sudo — the
mount command is printed so you can run it yourself if the automatic step
fails). Other platforms can still `serve`.

## Development

```
cargo build
cargo clippy --all-targets
cargo test                          # includes a subprocess bridge test, no OS mount
cargo test --test mount -- --ignored  # the real OS-mount round trip, run by hand
```

The wire format is pinned by golden tests (`wire_constants_are_pinned`,
`type_bytes_are_pinned_wire_format`, `swarm_id_wire_format_is_pinned`) — if one
of those fails after a change, you broke compatibility with already-issued
tickets and with peers running an older build.

## The web client

`share.agent-habilis.com` is a **pure static site**: no backend, no signalling
server, no database. Open `share.agent-habilis.com/files/<🐝ticket>` and the
browser connects straight to the producer. Session info is at
`/info/<🐝ticket>`.

The ticket is a bearer capability in the path so those views are ordinary
shareable URLs. The static host must fall back to `index.html` for deep links.

### How a browser reaches a peer behind NAT

Two connections, and the split is load-bearing:

1. The browser dials `agent-share/webrtc-signal/1` **over the iroh relay** and
   swaps one JSEP envelope each way.
2. It then opens a **fresh** connection to `agent-share/mount/1` against an
   address carrying only the WebRTC custom addr.

It has to be two, because iroh only fans a connect's Initial out to candidate
paths while the remote has no selected path — a live connection cannot be
upgraded onto a newly attached transport.

**The relay is a rendezvous, not a transport.** It carries the SDP exchange and
never a byte of file data. The cost is that there is no data fallback: when ICE
fails the transfer fails, and a symmetric-NAT-to-symmetric-NAT pair cannot
connect at all. That is the price of the guarantee.

### Building

```
cargo task web-wasm          # crates/agent-share-wasm-client/dist/{web,nodejs}
cd web && bun install && bun run dev
```

`web/` is the browser app, `node/` the `npx agent-share <🐝…>` receiver.
Both consume the same `.wasm`; only the wasm-bindgen glue differs.

Note `npx` needs a native WebRTC addon (`node-datachannel`), because Node has
no `RTCPeerConnection` and the relay will not carry data. The native binary
needs no addon and can mount the share as a filesystem.
