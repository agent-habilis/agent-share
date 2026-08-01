//! QUIC datagrams over a `WebRTC` data channel, as an iroh custom transport —
//! on the host and in the browser.
//!
//! One data channel per remote peer, one QUIC datagram per binary SCTP
//! message, no extra framing. The channel is negotiated **unreliable and
//! unordered** (`maxRetransmits: 0`): QUIC above it owns loss recovery and
//! congestion control, so reliable ordered SCTP underneath would stack a
//! second retransmission loop on every loss. Structure follows
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
//! The iroh relay is still rendezvous-only for the SDP exchange. The browser
//! backend may add a short-lived public TURN server to `iceServers` so ICE
//! itself can relay when LAN/mDNS and NAT hairpin both fail. The host/`str0m`
//! backend has no TURN client yet.

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
    BrowserHubTransport, BrowserRtcTransport, BrowserSession, IceServer, IceServers,
    PendingAnswer as BrowserPendingAnswer, PendingOffer as BrowserPendingOffer,
    answer as browser_answer, log_signal_sdps, offer as browser_offer,
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
    inner: std::sync::Arc<BrowserHubTransport>,
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
    /// Wrap a browser hub transport (consumer or producer).
    #[must_use]
    pub fn new(transport: std::sync::Arc<BrowserHubTransport>) -> Self {
        Self { inner: transport }
    }

    /// Empty hub for `local`, ready to register and later [`Self::attach`].
    #[must_use]
    pub fn hub(local: iroh_base::EndpointId) -> Self {
        Self::new(BrowserHubTransport::new(local))
    }

    /// The transport to register with `Builder::add_custom_transport`.
    #[must_use]
    pub fn transport(&self) -> std::sync::Arc<BrowserHubTransport> {
        std::sync::Arc::clone(&self.inner)
    }

    /// Attach a negotiated browser session for `remote`.
    ///
    /// Prefer calling [`BrowserPendingOffer::complete`] /
    /// [`BrowserPendingAnswer::complete`], which attach themselves; this is
    /// the escape hatch when the session pieces are already in hand.
    pub fn attach_parts(
        &self,
        remote: iroh_base::EndpointId,
        peer_connection: web_sys::RtcPeerConnection,
        data_channel: web_sys::RtcDataChannel,
        callbacks: Vec<wasm_bindgen::JsValue>,
    ) -> Result<(), String> {
        self.inner
            .attach(remote, peer_connection, data_channel, callbacks)
    }
}
