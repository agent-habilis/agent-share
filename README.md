# swarm-mount

`ahsw-mount` — share a folder with peers, or mount a peer's folder locally.
Read-only, lazy, no daemon: the producer serves file bytes on demand over an
[iroh](https://github.com/n0-computer/iroh) QUIC connection, and the consumer
mounts them through a loopback `NFSv3` bridge — the OS's built-in NFS client,
no FUSE, no kernel extension.

Extracted from [agent-habilis/swarm](https://github.com/agent-habilis/swarm)'s
`ahsw mount`. Tickets interoperate: a `🐝…` ticket minted by `ahsw mount serve`
mounts with `ahsw-mount`, and vice versa.

## Usage

Share a folder (producer):

```
ahsw-mount serve <dir>
```

This prints the consumer's ready-to-run command, carrying a `🐝…` bearer
ticket. The tree is scanned once at startup (a metadata-only snapshot); peers
fetch file bytes on demand as they read them. Serve keeps running until
interrupted.

Mount it (consumer):

```
ahsw-mount <🐝…> <mountpoint>
```

The mountpoint is created if missing (an existing directory must be empty) and
unmounted on Ctrl-C. Writes fail — the mount is read-only.

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

Wire compatibility with `ahsw` is pinned by golden tests
(`wire_constants_are_pinned`, `type_bytes_are_pinned_wire_format`,
`swarm_id_wire_format_is_pinned`) — if one of those fails after a change,
cross-tool interop broke.
