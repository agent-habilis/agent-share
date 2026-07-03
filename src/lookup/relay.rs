//! The relay leg of the lookup layer, vendored from agent-habilis/swarm's
//! `src/lookup/relay.rs` — trimmed to [`relay_mode`], the mapping from a
//! [`RelayChoice`] to an iroh [`RelayMode`]. The rung-selection/failover
//! machinery (beacon-only) is not needed by mount.

use iroh::{RelayMode, endpoint::default_relay_mode};

use crate::protocol::swarm::RelayChoice;

/// Map a [`RelayChoice`] to the iroh [`RelayMode`] for an endpoint.
///
/// A participant on `Pinned` uses iroh's resilient multi-relay
/// `default_relay_mode()` (the n0 prod set): pinning a participant to
/// one relay made `bind()` block on that relay's handshake and dropped
/// iroh's relay fallback. A `Custom` ladder pins the endpoint to that
/// whole set.
pub(super) fn relay_mode(choice: &RelayChoice) -> RelayMode {
    match choice {
        RelayChoice::Disabled => {
            tracing::debug!("relay disabled (not in the lookup allowlist)");
            RelayMode::Disabled
        }
        RelayChoice::Custom(ladder) => {
            tracing::debug!(rungs = ladder.len(), "endpoint pinned to relay ladder");
            RelayMode::custom(ladder.iter().cloned())
        }
        RelayChoice::Pinned => {
            tracing::debug!("participant on iroh resilient multi-relay default");
            default_relay_mode()
        }
    }
}
