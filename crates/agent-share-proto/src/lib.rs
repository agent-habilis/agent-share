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
//! - [`auth`]: the share token — what that secret becomes once a password is in
//!   play, and what actually goes on the wire.
//! - [`manifest`]: the directory/file listing a consumer turns into a tree.
//! - [`framing`]: the ALPN, the op codes, and the request/response byte
//!   layouts.
//! - [`lookup`]: the discovery allowlist baked into a ticket.
//! - [`peer_addr`]: the `EndpointAddr` JSON codec the ticket embeds.
//! - [`roster`]: who is on a share's mesh, joining the meta document's cards
//!   to the engine's roster of who is actually present.
//!
//! Everything here is wire format. Breaking it is *permitted* — there is no
//! compatibility promise — but it must be **deliberate**, and that is what the
//! golden tests (`wire_constants_are_pinned`,
//! `type_bytes_are_pinned_wire_format`) are for. Two builds of the same version
//! still have to agree, so a constant that shifts as a refactor's side effect
//! is a silent break between a producer on `main` and a consumer on a branch.
//! The tests live beside the code they pin and must fail loudly; when a change
//! really is intended, update them in the same commit that makes it.

pub use self::auth::{MOUNT_TICKET_LABEL, ShareAuth, ct_eq, share_token};
pub use self::client::{
    CARD_APP, CARD_CLIENT, CARD_ENDPOINT, CARD_ROLE, CARD_RUNTIME, CARD_TRANSPORT, CARD_VERSION,
    PRODUCT as CLIENT_PRODUCT, PeerCard, format_label as format_client_label,
};
pub use self::framing::{
    BENCH_ECHO_INTERVAL_SECS, BENCH_KIND_ECHO, BENCH_KIND_FILL, CLOSE_BAD_SECRET,
    CLOSE_UNAUTHORIZED, DEFAULT_BENCH_DURATION_SECS, MAX_BENCH_ECHO_BYTES, MAX_BENCH_FILL_BYTES,
    MAX_MANIFEST_BYTES, MAX_READ_LEN, MOUNT_ALPN, OP_BENCH, OP_MANIFEST, OP_READ,
    REQUEST_HEADER_LEN, SECRET_LEN, WEBRTC_SIGNAL_ALPN,
};
pub use self::manifest::{DirEntry, FileEntry, MountManifest, ReadStatus};
pub use self::ticket::{
    MountTicket, TICKET_FLAG_PASSWORD, TICKET_KIND_BENCH_QUIC, TICKET_KIND_BENCH_RELAY,
    TICKET_KIND_BENCH_WEBRTC, TICKET_KIND_SHARE,
};

pub mod auth;
/// Who may change a share: the creator's signature over each manifest version.
pub mod authorship;
pub mod client;
pub mod framing;
pub mod lookup;
pub mod manifest;
pub mod mesh_key;
pub mod peer_addr;
pub mod roster;
pub mod serving;
pub mod ticket;
pub mod token;
