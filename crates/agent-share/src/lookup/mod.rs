//! The lookup layer, vendored from agent-habilis/swarm's `src/lookup/mod.rs` —
//! trimmed to building the iroh endpoint and wiring the selected lookups onto
//! it. Each lookup mechanism lives in its own submodule — [`mdns`] (LAN
//! multicast), [`dht`] (mainline DHT), and [`relay`] (the relay-mode mapping).
//! The gossip/beacon machinery (`build_swarm`, `probe_connect`, capability) is not
//! needed by mount.

use std::net::{Ipv4Addr, SocketAddrV4};

use anyhow::{Context, Result};
use iroh::address_lookup::memory::MemoryLookup;
use iroh::{
    Endpoint, EndpointAddr, RelayMode, SecretKey,
    endpoint::{PortmapperConfig, presets},
};

use crate::protocol::swarm::{LookupOpts, RelayChoice};

mod dht;
mod mdns;
mod relay;

/// Build an iroh endpoint for a swarm's lookups.
///
/// - `lookups`: which address-lookups (mDNS / DHT) and relay to wire.
///   When any lookup is on, the builder is composed from
///   `presets::Minimal` plus the selected lookups; the relay maps via
///   [`relay::relay_mode`]. An all-off (loopback-only) set wires none of
///   them.
/// - `secret_key`: `Some` pins a deterministic identity; `None` lets iroh
///   generate a fresh random key (the normal participant endpoint).
/// - `bind_port`: loopback-only — `Some(port)` binds `127.0.0.1:port`.
///   `None` binds an ephemeral port. Ignored when lookups are on (N0
///   manages binding).
pub(crate) async fn build_endpoint(
    lookups: &LookupOpts,
    secret_key: Option<SecretKey>,
    bind_port: Option<u16>,
    alpns: Vec<Vec<u8>>,
    webrtc: Option<webrtc_transport::WebRtcHandle>,
) -> Result<Endpoint> {
    // A pinned key alone no longer means "beacon": a producer pins one so the
    // WebRTC transport can advertise the same identity the endpoint binds.
    let is_beacon = secret_key.is_some() && webrtc.is_none();
    let network = lookups.network_label();
    let mut builder = if lookups.is_loopback() {
        debug_assert!(
            !lookups.mdns && !lookups.dht && lookups.relay == RelayChoice::Disabled,
            "loopback-only swarm must resolve to all-off lookups"
        );
        // Loopback-only = strictly loopback, **zero external network calls**.
        // `Minimal` picks the rustls crypto provider without N0's
        // DNS/relay defaults; we then lock down every path that could
        // touch a non-loopback host: `bind_addr` 127.0.0.1,
        // `RelayMode::Disabled` (no relay; no address-lookup is wired
        // for a loopback-only swarm so no DNS/pkarr/mDNS/DHT either), and
        // `PortmapperConfig::Disabled` — the one remaining default-on
        // reach (UPnP/PCP/NAT-PMP to the LAN gateway, on even with the
        // relay off). With relay + portmapper off, iroh's netcheck has
        // no external targets (local-interface report only).
        Endpoint::builder(presets::Minimal)
            .bind_addr(SocketAddrV4::new(
                Ipv4Addr::LOCALHOST,
                bind_port.unwrap_or(0),
            ))
            .context("failed to set bind address")?
            .relay_mode(RelayMode::Disabled)
            .portmapper_config(PortmapperConfig::Disabled)
    } else {
        // `Minimal` (not `presets::N0`): N0-DNS is intentionally not
        // wired (the relay ladder is the fast path; DHT is the
        // operator-free eternal backstop). `Minimal` still sets the
        // rustls crypto provider. The mDNS / DHT address-lookups are
        // wired **after** bind (below) — in iroh 1.0 they live in
        // companion crates and need the bound endpoint's id.
        Endpoint::builder(presets::Minimal).relay_mode(relay::relay_mode(&lookups.relay))
    };

    if let Some(secret_key) = secret_key {
        builder = builder.secret_key(secret_key);
    }

    // ALPNs the endpoint accepts inbound connections for. The mount producer
    // passes its `MOUNT_ALPN` so it can `endpoint.accept()` directly.
    if !alpns.is_empty() {
        builder = builder.alpns(alpns);
    }

    // Additive, never a `Preset`: a preset would make WebRTC this endpoint's
    // *only* transport. Right for a browser, wrong for a native peer that
    // should still prefer iroh's own hole-punched paths — WebRTC is the
    // browser's only way in, and a fallback for NATs that defeat
    // hole-punching but not ICE.
    if let Some(handle) = webrtc {
        builder = builder.add_custom_transport(handle.transport());
    }

    // Transport config is intentionally left at iroh's defaults: iroh tunes
    // keep-alive / idle (and the per-path multipath settings) for its
    // holepunching, and its own docs warn that adjusting them "may cause
    // suboptimal usage".
    let endpoint = builder.bind().await.context("failed to bind endpoint")?;
    // Post-bind address-lookup wiring: in iroh 1.0 the mDNS / mainline-DHT
    // providers are companion crates built from the bound endpoint's id and
    // added to its lookup services. Loopback-only swarms wire none (asserted
    // above). The relay leg is configured pre-bind via `relay_mode`.
    if lookups.mdns {
        mdns::wire(&endpoint)?;
    }
    if lookups.dht {
        dht::wire(&endpoint)?;
    }
    tracing::info!(
        network,
        mdns = lookups.mdns,
        dht = lookups.dht,
        relay = ?lookups.relay,
        role = if is_beacon { "beacon" } else { "participant" },
        endpoint_id = %endpoint.id(),
        "endpoint bound"
    );
    Ok(endpoint)
}

/// Register a peer's address so the endpoint can connect to it.
///
/// # Errors
/// The endpoint has no memory address-lookup registered to add to.
pub fn add_peer_addr(endpoint: &Endpoint, addr: EndpointAddr) -> Result<()> {
    let lookup = MemoryLookup::new();
    lookup.add_endpoint_info(addr);
    endpoint.address_lookup()?.add(lookup);
    tracing::debug!("registered a direct peer address with the endpoint");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{LookupOpts, build_endpoint};

    // Binds the `Minimal`-based reachable branch (the default relay
    // ladder, no lookup wired) and the loopback all-off branch. mDNS
    // multicast / mainline-DHT socket setup is environment-dependent, so
    // it is not exercised here.

    #[tokio::test]
    async fn loopback_all_off_binds() {
        let endpoint = build_endpoint(&LookupOpts::loopback(), None, None, Vec::new(), None)
            .await
            .expect("loopback endpoint must bind");
        endpoint.close().await;
    }

    #[tokio::test]
    async fn public_default_relay_binds() {
        // No lookup wired: exercises the `Minimal` + pinned-ladder
        // composition. `bind()` is non-blocking wrt the relay, so this
        // is offline-safe even with the relay ladder configured.
        let endpoint = build_endpoint(&LookupOpts::public_preset(), None, None, Vec::new(), None)
            .await
            .expect("endpoint with pinned relay ladder must bind");
        endpoint.close().await;
    }
}
