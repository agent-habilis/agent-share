//! The relay leg of the lookup layer, vendored from agent-habilis/swarm's
//! `src/lookup/relay.rs` — trimmed to [`relay_mode`], the mapping from a
//! [`RelayChoice`] to an iroh [`RelayMode`]. The rung-selection/failover
//! machinery (beacon-only) is not needed by mount.

use fofoca::iroh::RelayMode;

use crate::protocol::swarm::RelayChoice;

/// Map a [`RelayChoice`] to the iroh [`RelayMode`] for an endpoint.
///
/// A participant on `Pinned` gets [`pinned_relay_ladder`] — **our relay first,
/// n0's as fallback** — rather than iroh's `default_relay_mode()` (n0 only).
/// It stays a *ladder* rather than a single rung for the reason the previous
/// comment recorded: pinning a participant to one relay made `bind()` block on
/// that relay's handshake and dropped iroh's relay fallback. A `Custom` ladder
/// pins the endpoint to that whole set.
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
            let ladder = pinned_ladder();
            tracing::debug!(rungs = ladder.len(), "participant on the pinned ladder");
            RelayMode::custom(ladder)
        }
    }
}

/// The `Pinned` ladder: **our relay first, n0's as fallback**.
///
/// Sourced from `agent-habilis-mesh` rather than restated here. A ticket that
/// says "pinned" carries no URLs, so both ends resolve the name independently —
/// two copies of this list that drifted would put two peers on different relays
/// with nothing to say why they never met. The mesh derived from a share homes
/// on the same rungs for the same reason.
fn pinned_ladder() -> Vec<fofoca::iroh::RelayUrl> {
    fofoca::RENDEZVOUS_RELAY_LADDER
        .iter()
        .map(|raw| {
            raw.parse()
                .expect("RENDEZVOUS_RELAY_LADDER entries are valid relay URLs")
        })
        .collect()
}
