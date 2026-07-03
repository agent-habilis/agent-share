//! Wire types, vendored from agent-habilis/swarm's `src/protocol/` —
//! trimmed to what mount needs.
//!
//! - [`swarm`]: the `🐝…` identifier (`SwarmId` shallow string) + the
//!   lookup allowlist + its extractor / relay-ladder parsing.
//! - [`token`]: the `🐝…` ticket framing (base58check + type byte).
//! - [`peer_addr`]: the endpoint-address JSON codec.

pub(crate) mod peer_addr;
pub(crate) mod swarm;
pub(crate) mod token;

pub(crate) use swarm::SwarmId;
