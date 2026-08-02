# agent-share-proto

The `agent-share` wire format: the ticket codec, the mount manifest, and
the request/response framing.

This crate exists so the CLI producer, the CLI (NFS) consumer, and the browser
client link **one** implementation of the bytes rather than three that drift.
It is deliberately transport-free — no `iroh::Endpoint`, no `tokio`, no
filesystem — so it compiles to `wasm32-unknown-unknown` unchanged. Callers own
their streams and hand slices here.

Everything in it is wire format. Changing a byte layout breaks every
already-issued ticket and every peer on an older build, so the golden tests
(`wire_constants_are_pinned`, `type_bytes_are_pinned_wire_format`,
`flag_bytes_are_pinned_wire_format`) sit beside the code they pin. If one
fails, you broke compatibility — fix the change, don't update the test.

`cargo task ci` runs `cargo check --target wasm32-unknown-unknown -p
agent-share-proto` so a host-only dependency can't sneak in unnoticed.
