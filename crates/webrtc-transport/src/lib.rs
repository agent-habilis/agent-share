//! QUIC datagrams over a `WebRTC` data channel, as an iroh custom transport —
//! on the host and in the browser.
//!
//! One data channel per remote peer, one QUIC datagram per binary SCTP
//! message, no extra framing. Structure follows
//! [iroh-multihop-transport](https://github.com/agent-habilis/agent-gossip)
//! and the upstream
//! [iroh-tor-transport](https://github.com/n0-computer/iroh-tor-transport):
//! a transport factory, a per-endpoint receiver, a registry-backed sender, and
//! a per-session driver pumping the connection.
//!
//! # Why one crate with two backends
//!
//! `str0m` and `tokio` do not target `wasm32`; `web-sys` does not exist off the
//! browser. The *drivers* therefore cannot be one implementation. But the
//! protocol — the JSEP envelope, the transport id, the address convention — is
//! identical, and it is exactly the part that must not drift: two peers that
//! disagree about the transport id or the envelope shape fail to connect with
//! no useful error.
//!
//! So the protocol half lives at the crate root, always compiled and free of
//! host- and browser-only dependencies, and each backend sits behind a feature:
//!
//! - `host` — sans-io [`str0m`] driven by tokio, plus STUN gathering ([`stun`]).
//! - `web` — the browser's own `RTCPeerConnection` through `web-sys`.
//!
//! Neither is on by default; a consumer enables the one matching its target.
//!
//! # NAT traversal
//!
//! `str0m` gathers no candidates — it owns no sockets, so discovery is the
//! caller's job. Skipping it (as the experiment this was ported from did) means
//! advertising a single host candidate on a private interface, which connects
//! on one LAN and nowhere else. [`stun`] closes that on the host side; the
//! browser's ICE agent does it natively once given `iceServers`.
//!
//! There is no TURN client. `agent-share` treats the relay as a rendezvous for
//! the SDP exchange only, so a failed negotiation is a hard error rather than a
//! quiet downgrade onto someone else's infrastructure.

mod addr;
mod signaling;

pub use addr::{WEBRTC_TRANSPORT_ID, custom_addr, parse_custom_addr};
pub use signaling::{MAX_ENVELOPE_BYTES, SIGNAL_VERSION, SignalEnvelope};

/// Label of the single data channel each session carries. Both ends must use
/// the same string or the channel never opens.
pub const DATA_CHANNEL_LABEL: &str = "iroh";

#[cfg(feature = "host")]
mod host;

#[cfg(feature = "host")]
pub use host::{
    IceConfig, NegotiatedSession, PendingAnswer, PendingOffer, WebRtcTransport, answer,
    answer_with, offer, offer_with, stun,
};

#[cfg(feature = "web")]
mod web;

#[cfg(feature = "web")]
pub use web::{
    BrowserRtcTransport, BrowserSession, IceServers, PendingOffer as BrowserPendingOffer,
    offer as browser_offer,
};

/// A registered `WebRTC` transport, ready to hand to an iroh endpoint builder.
///
/// The same name on both targets so a consumer's wiring code is written once:
/// which backend it wraps is decided by the feature, not by the caller. Cheap
/// to clone — it is a handle, and every clone shares one session registry.
#[cfg(any(feature = "host", feature = "web"))]
#[derive(Debug, Clone)]
pub struct WebRtcHandle {
    #[cfg(feature = "host")]
    inner: std::sync::Arc<WebRtcTransport>,
    #[cfg(all(feature = "web", not(feature = "host")))]
    inner: std::sync::Arc<BrowserRtcTransport>,
}

#[cfg(feature = "host")]
impl WebRtcHandle {
    /// Wrap a host transport.
    #[must_use]
    pub fn new(transport: std::sync::Arc<WebRtcTransport>) -> Self {
        Self { inner: transport }
    }

    /// The transport to register with `Builder::add_custom_transport`.
    ///
    /// Registration is deliberately additive rather than a `Preset`: a preset
    /// would make `WebRTC` the endpoint's *only* transport, which is right for a
    /// browser and wrong for a native peer that should still prefer iroh's own
    /// hole-punched paths.
    #[must_use]
    pub fn transport(&self) -> std::sync::Arc<WebRtcTransport> {
        std::sync::Arc::clone(&self.inner)
    }

    /// Attach a negotiated session for `remote`.
    ///
    /// # Errors
    /// The registry rejects the session (already attached).
    pub fn attach(
        &self,
        remote: iroh_base::EndpointId,
        session: NegotiatedSession,
    ) -> anyhow::Result<()> {
        self.inner.attach(remote, session)
    }
}

#[cfg(all(feature = "web", not(feature = "host")))]
impl WebRtcHandle {
    /// Wrap a browser transport.
    #[must_use]
    pub fn new(transport: std::sync::Arc<BrowserRtcTransport>) -> Self {
        Self { inner: transport }
    }

    /// The transport to register with `Builder::add_custom_transport`.
    #[must_use]
    pub fn transport(&self) -> std::sync::Arc<BrowserRtcTransport> {
        std::sync::Arc::clone(&self.inner)
    }
}
