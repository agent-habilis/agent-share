//! Browser-side JSEP: drive `RTCPeerConnection` to a negotiated data channel.
//!
//! Deliberately carrier-agnostic, exactly like the host side. [`offer`] hands
//! back a [`SignalEnvelope`] and the caller carries it however it likes — over
//! an iroh connection, over HTTP, in a QR code. Nothing here knows how the
//! remote is reached, which is what lets the browser reuse the same signal
//! ALPN as everything else instead of needing a web-specific channel.
//!
//! Vanilla ICE: candidates ride inside the SDP, so gathering must complete
//! *before* the envelope goes out. There is no trickle message to add them
//! later.

use futures::channel::mpsc;
use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    MessageEvent, RtcConfiguration, RtcDataChannel, RtcIceGatheringState, RtcPeerConnection,
    RtcSdpType, RtcSessionDescriptionInit,
};

use crate::{DATA_CHANNEL_LABEL, SIGNAL_VERSION, SignalEnvelope};

use super::transport::{BrowserRtcTransport, IN_QUEUE};
use iroh_base::EndpointId;

/// How long to wait for the data channel after the answer is applied.
const CHANNEL_OPEN_ATTEMPTS: u32 = 1200;
/// Poll interval for the two readiness waits below.
const POLL_MS: i32 = 50;
/// Stop queueing into the channel above this much buffered data; QUIC above
/// retransmits, and an unbounded buffer is worse than a dropped datagram.
const BUFFER_CAP: u32 = 1 << 20;

/// STUN servers for the browser's own ICE agent.
///
/// The browser gathers server-reflexive candidates itself once given these —
/// the counterpart to the host side's hand-rolled `stun` module, which exists
/// only because `str0m` gathers nothing. Passing an empty list yields host
/// candidates only, which reaches one LAN and nowhere else.
#[derive(Debug, Clone)]
pub struct IceServers(pub Vec<String>);

impl Default for IceServers {
    fn default() -> Self {
        Self(vec![
            "stun:stun.l.google.com:19302".to_owned(),
            "stun:stun.cloudflare.com:3478".to_owned(),
        ])
    }
}

impl IceServers {
    /// No STUN — host candidates only.
    #[must_use]
    pub fn host_only() -> Self {
        Self(Vec::new())
    }

    fn to_configuration(&self) -> RtcConfiguration {
        let config = RtcConfiguration::new();
        let servers = js_sys::Array::new();
        for url in &self.0 {
            let entry = js_sys::Object::new();
            let _ =
                js_sys::Reflect::set(&entry, &JsValue::from_str("urls"), &JsValue::from_str(url));
            servers.push(&entry);
        }
        config.set_ice_servers(&servers);
        config
    }
}

/// A negotiated browser session: the peer connection, its data channel, and
/// the transport bridging it into iroh.
pub struct BrowserSession {
    pub transport: std::sync::Arc<BrowserRtcTransport>,
    peer_connection: RtcPeerConnection,
    data_channel: RtcDataChannel,
    // Keeps the `onmessage` closure alive for the session's lifetime.
    _callbacks: Vec<JsValue>,
}

impl std::fmt::Debug for BrowserSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BrowserSession")
            .finish_non_exhaustive()
    }
}

/// Offerer state between producing the offer and applying the answer.
pub struct PendingOffer {
    local: EndpointId,
    peer_connection: RtcPeerConnection,
    data_channel: RtcDataChannel,
    in_rx: mpsc::Receiver<Vec<u8>>,
    callbacks: Vec<JsValue>,
}

impl std::fmt::Debug for PendingOffer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PendingOffer")
            .finish_non_exhaustive()
    }
}

/// Start a negotiation as the offerer. Carry the returned envelope to the
/// remote by any means, then feed its answer to [`PendingOffer::complete`].
///
/// # Errors
/// The peer connection cannot be created, or SDP generation fails.
pub async fn offer(
    local: EndpointId,
    ice: &IceServers,
) -> Result<(PendingOffer, SignalEnvelope), JsValue> {
    let peer_connection = RtcPeerConnection::new_with_configuration(&ice.to_configuration())
        .map_err(|error| js_err("RTCPeerConnection", error))?;
    let data_channel = peer_connection.create_data_channel(DATA_CHANNEL_LABEL);
    data_channel.set_binary_type(web_sys::RtcDataChannelType::Arraybuffer);

    // Inbound: data-channel messages become QUIC datagrams.
    let (in_tx, in_rx) = mpsc::channel::<Vec<u8>>(IN_QUEUE);
    let onmessage = {
        let mut in_tx = in_tx.clone();
        Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
            if let Ok(buffer) = event.data().dyn_into::<js_sys::ArrayBuffer>() {
                let bytes = js_sys::Uint8Array::new(&buffer).to_vec();
                // Lossy on overflow, like the UDP it stands in for.
                let _ = in_tx.try_send(bytes);
            }
        })
    };
    data_channel.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));

    let sdp_offer = JsFuture::from(peer_connection.create_offer())
        .await
        .map_err(|error| js_err("createOffer", error))?;
    let offer_init: RtcSessionDescriptionInit = sdp_offer.unchecked_into();
    JsFuture::from(peer_connection.set_local_description(&offer_init))
        .await
        .map_err(|error| js_err("setLocalDescription", error))?;
    // Vanilla ICE: wait for gathering so the single envelope carries every
    // candidate. Nothing can be added after this point.
    wait_ice_complete(&peer_connection).await;
    let local_sdp = peer_connection
        .local_description()
        .ok_or_else(|| JsValue::from_str("no local description after gathering"))?
        .sdp();

    let envelope = SignalEnvelope::Offer {
        version: SIGNAL_VERSION,
        endpoint_id: local.to_string(),
        sdp: local_sdp,
    };
    Ok((
        PendingOffer {
            local,
            peer_connection,
            data_channel,
            in_rx,
            callbacks: vec![onmessage.into_js_value()],
        },
        envelope,
    ))
}

impl PendingOffer {
    /// Apply the remote answer and wait for the data channel to open.
    ///
    /// # Errors
    /// A non-answer envelope, a bad SDP, or the channel never opening.
    pub async fn complete(self, answer: &SignalEnvelope) -> Result<BrowserSession, JsValue> {
        let Self {
            local,
            peer_connection,
            data_channel,
            in_rx,
            callbacks,
        } = self;

        let remote = answer
            .claimed_endpoint()
            .map_err(|error| any_err("answer endpoint id", error))?;
        let SignalEnvelope::Answer { sdp, .. } = answer else {
            return Err(JsValue::from_str("expected an answer envelope"));
        };

        let answer_init = RtcSessionDescriptionInit::new(RtcSdpType::Answer);
        answer_init.set_sdp(sdp);
        JsFuture::from(peer_connection.set_remote_description(&answer_init))
            .await
            .map_err(|error| js_err("setRemoteDescription", error))?;

        wait_channel_open(&data_channel).await?;

        // Outbound: a same-thread pump drains QUIC datagrams into the channel.
        // `buffered_amount` is the backpressure signal.
        let (transport, mut out_rx) = BrowserRtcTransport::new(local, remote, in_rx);
        {
            use futures::StreamExt as _;
            let data_channel = data_channel.clone();
            wasm_bindgen_futures::spawn_local(async move {
                while let Some(datagram) = out_rx.next().await {
                    if data_channel.buffered_amount() < BUFFER_CAP {
                        let _ = data_channel.send_with_u8_array(&datagram);
                    }
                }
            });
        }

        Ok(BrowserSession {
            transport,
            peer_connection,
            data_channel,
            _callbacks: callbacks,
        })
    }
}

impl BrowserSession {
    /// True while the channel is still usable.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.data_channel.ready_state() == web_sys::RtcDataChannelState::Open
    }

    /// Tear the session down explicitly rather than waiting for drop.
    pub fn close(&self) {
        self.data_channel.close();
        self.peer_connection.close();
    }
}

fn js_err(context: &str, error: JsValue) -> JsValue {
    JsValue::from_str(&format!("{context}: {error:?}"))
}

fn any_err(context: &str, error: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&format!("{context}: {error}"))
}

async fn wait_ice_complete(peer_connection: &RtcPeerConnection) {
    // No timeout: gathering completes on its own once every server has
    // answered or timed out internally, and cutting it short would drop
    // candidates we cannot add later.
    loop {
        if peer_connection.ice_gathering_state() == RtcIceGatheringState::Complete {
            return;
        }
        sleep_ms(POLL_MS).await;
    }
}

async fn wait_channel_open(data_channel: &RtcDataChannel) -> Result<(), JsValue> {
    for _ in 0..CHANNEL_OPEN_ATTEMPTS {
        match data_channel.ready_state() {
            web_sys::RtcDataChannelState::Open => return Ok(()),
            web_sys::RtcDataChannelState::Closing | web_sys::RtcDataChannelState::Closed => {
                return Err(JsValue::from_str("data channel closed during setup"));
            }
            _ => sleep_ms(POLL_MS).await,
        }
    }
    Err(JsValue::from_str("timed out waiting for the data channel"))
}

async fn sleep_ms(millis: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        if let Some(window) = web_sys::window() {
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, millis);
        }
    });
    let _ = JsFuture::from(promise).await;
}
