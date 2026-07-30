//! The `agent-share` wire format — everything both ends of a share must
//! agree on, and nothing else.
//!
//! This crate exists so the CLI producer, the CLI (NFS) consumer, and the
//! browser client link **one** implementation of the bytes rather than three
//! that drift. It is deliberately transport-free: no `iroh::Endpoint`, no
//! `tokio`, no filesystem. Callers own the streams and hand slices here.
//!
//! - [`token`]: the branded `🐝` Base58Check framing every agent-habilis
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

pub mod framing;
pub mod lookup;
pub mod manifest;
pub mod peer_addr;
pub mod ticket;
pub mod token;

pub use framing::{
    MAX_MANIFEST_BYTES, MAX_READ_LEN, MOUNT_ALPN, OP_MANIFEST, OP_READ, REQUEST_HEADER_LEN,
    SECRET_LEN, WEBRTC_SIGNAL_ALPN,
};
pub use manifest::{DirEntry, FileEntry, MountManifest, ReadStatus};
pub use ticket::MountTicket;
