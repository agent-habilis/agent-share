//! The `agent-share` wire format — everything both ends of a share must
//! agree on, and nothing else.
//!
//! This crate exists so the CLI producer, the CLI (NFS) consumer, and the
//! browser client link **one** implementation of the bytes rather than three
//! that drift. It is deliberately transport-free: no `iroh::Endpoint`, no
//! `tokio`, no filesystem. Callers own the streams and hand slices here.
//!
//! - [`token`]: the Base58Check framing every agent-habilis
//!   token shares.
//! - [`ticket`]: the mount ticket — a bearer secret plus how to reach the
//!   producer.
//! - [`manifest`]: the directory/file listing a consumer turns into a tree.
//! - [`framing`]: the ALPN, the op codes, and the request/response byte
//!   layouts.
//! - [`lookup`]: the discovery allowlist baked into a ticket.
//! - [`peer_addr`]: the `EndpointAddr` JSON codec the ticket embeds.
//!
//! Everything here is wire format. A change to any byte layout breaks every
//! already-issued ticket and every peer on an older build, so the golden
//! tests (`wire_constants_are_pinned`, `type_bytes_are_pinned_wire_format`)
//! live beside the code they pin and must fail loudly rather than be updated
//! to match.

pub mod client;
pub mod framing;
pub mod lookup;
pub mod manifest;
pub mod mesh_key;
pub mod peer_addr;
pub mod ticket;
pub mod token;

pub use client::{
    CARD_APP, CARD_CLIENT, CARD_ENDPOINT, CARD_ROLE, CARD_RUNTIME, CARD_TRANSPORT, CARD_VERSION,
    PRODUCT as CLIENT_PRODUCT, PeerCard, format_label as format_client_label,
};

pub use framing::{
    BENCH_ECHO_INTERVAL_SECS, BENCH_KIND_ECHO, BENCH_KIND_FILL, DEFAULT_BENCH_DURATION_SECS,
    MAX_BENCH_ECHO_BYTES, MAX_BENCH_FILL_BYTES, MAX_MANIFEST_BYTES, MAX_READ_LEN, MOUNT_ALPN,
    OP_BENCH, OP_MANIFEST, OP_READ, REQUEST_HEADER_LEN, SECRET_LEN, WEBRTC_SIGNAL_ALPN,
};
pub use manifest::{DirEntry, FileEntry, MountManifest, ReadStatus};
pub use ticket::{
    MountTicket, TICKET_FLAG_BENCH_RELAY, TICKET_FLAG_BENCH_WEBRTC, TICKET_FLAG_NONE,
};
