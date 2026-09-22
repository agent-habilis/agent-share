//! Wire types.
//!
//! The parts both ends of a share must agree on byte-for-byte live in
//! [`agent_share_proto`] so the browser client links the same code; this
//! module re-exports them under their long-standing paths and keeps only what
//! is genuinely CLI-side.
//!
//! - [`swarm`]: the swarm identifier (`SwarmId` shallow string) + the
//!   `--mdns/--dht/--relay` flag resolution. Host-only: a browser has no
//!   flags to resolve.
//!
//! The token codec and the `EndpointAddr` JSON codec now live in
//! `agent_share_proto::{token, peer_addr}`, reached through
//! `agent_share_proto::ticket` rather than re-exported here — nothing in the
//! binary touches them directly any more.

pub(crate) use self::swarm::SwarmId;

pub(crate) mod swarm;
