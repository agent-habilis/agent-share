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
/// on the same rungs for the same reason, so `mount::sources` dials seeders
/// through this one function rather than parsing the constant a second time.
///
/// The engine parses the list, not us: `relay_ladder` is `LazyLock`-cached and
/// its `RelayUrl`s are `Arc`-backed, so this costs a clone rather than five
/// URL parses per endpoint.
pub(crate) fn pinned_ladder() -> Vec<fofoca::iroh::RelayUrl> {
    fofoca::net::relay_ladder(&fofoca::protocol::RelayChoice::Pinned)
}

#[cfg(test)]
mod tests {
    use super::{RelayChoice, pinned_ladder, relay_mode};

    /// Rung 0 is our own relay, and iroh's `default_relay_mode()` does not name
    /// it — an endpoint built on the default could never home there. The browser
    /// producer was built that way, so it advertised a "pinned" ticket while
    /// sitting on n0's relays, and a tab-to-tab pair started on two different
    /// ones. Compared as sets: `RelayMap` is a `BTreeMap`, so rung order does
    /// not survive, and nothing downstream wants it to — iroh picks a home relay
    /// by measured latency, never by position.
    #[test]
    fn a_pinned_endpoint_is_offered_our_relay() {
        let ladder = pinned_ladder();
        let ours = ladder.first().expect("the pinned ladder has rungs").clone();
        assert_eq!(ours.host_str(), Some("relay.agent-habilis.com"));

        let mut offered = relay_mode(&RelayChoice::Pinned)
            .relay_map()
            .urls::<Vec<_>>();
        let mut expected = ladder;
        offered.sort();
        expected.sort();
        assert_eq!(offered, expected);
        assert!(offered.contains(&ours));
    }

    /// A loopback participant reaches no relay at all.
    #[test]
    fn a_disabled_choice_names_no_relay() {
        let offered = relay_mode(&RelayChoice::Disabled)
            .relay_map()
            .urls::<Vec<fofoca::iroh::RelayUrl>>();
        assert!(offered.is_empty());
    }
}
