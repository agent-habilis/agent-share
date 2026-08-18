# agent-share

The CLI: share a directory read-only over iroh QUIC, or consume one.

- **Produce** — `agent-share serve <dir>` scans the tree once (metadata only) and
  serves file bytes on demand, printing a bearer ticket.
- **Consume** — `agent-share <ticket> <target>` creates `agent-share-…/` under the
  target and mounts through a loopback NFSv3 bridge, so the OS's own NFS client
  does the filesystem work. No FUSE, no kernel extension, no daemon.

Reads are lazy and ranged: only the bytes a reader touches cross the network.

## Bench

Synthetic throughput / latency over `OP_BENCH` (no real directory):

```bash
agent-share bench --transport webrtc                 # producer
agent-share bench --transport relay
agent-share bench '<ticket>'                         # consumer (30s)
agent-share bench '<ticket>' --duration 30
```

`--transport` is set on the **producer** and encoded in the ticket; the consumer
has no transport flag. After connect the consumer measures for **30s** by default
(`--duration`). `relay` dials **only** the iroh relay URL (direct IPs stripped);
`webrtc` dials **only** the WebRTC custom addr. Same-machine WebRTC still uses
host ICE (localhost), so compare against a forced relay to see a real gap. The
browser lab at `packages/agent-share-app/src/lab/index.html` and `npx agent-share bench` expose the same pair.

## Where things live

This crate owns the mount protocol's *server and client*, plus the WebRTC lane that lets
a peer with no IP path reach it. The pieces it shares with the browser live elsewhere:

| | |
|---|---|
| `agent-share-proto` | the wire format — ticket, manifest, framing |
| `fofoca-iroh-webrtc-transport` | the iroh custom transport (`host` feature here) |
| `agent-share-wasm-client` | the same protocol, compiled to wasm for the browser app and `agent-share-node` |

See the repository README for the web client and the overall layout.

## Not published

`publish = false`. The ticket format and the `agent-share/mount/1` ALPN are pinned
by golden tests; both ends of a mount must run the same build.
