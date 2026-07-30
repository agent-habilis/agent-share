# agent-share

The CLI: share a directory read-only over iroh QUIC, or consume one.

- **Produce** — `agent-share serve <dir>` scans the tree once (metadata only) and
  serves file bytes on demand, printing a `🐝…` bearer ticket.
- **Consume** — `agent-share <🐝…> <mountpoint>` mounts it through a loopback NFSv3
  bridge, so the OS's own NFS client does the filesystem work. No FUSE, no kernel
  extension, no daemon.

Reads are lazy and ranged: only the bytes a reader touches cross the network.

## Where things live

This crate owns the mount protocol's *server and client*, plus the WebRTC lane that lets
a peer with no IP path reach it. The pieces it shares with the browser live elsewhere:

| | |
|---|---|
| `agent-share-proto` | the wire format — ticket, manifest, framing |
| `webrtc-transport` | the iroh custom transport (`host` feature here) |
| `agent-share-wasm-client` | the same protocol, compiled to wasm for `ui/` and `node/` |

See the repository README for the web client and the overall layout.

## Not published

`publish = false`. The `🐝` ticket format and the `agent-share/mount/1` ALPN are pinned
by golden tests; both ends of a mount must run the same build.
