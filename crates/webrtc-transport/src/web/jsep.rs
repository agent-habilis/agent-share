//! Browser-side JSEP: drive `RTCPeerConnection` to a negotiated data channel.
//!
//! Deliberately carrier-agnostic, exactly like the host side. [`offer`] /
//! [`answer`] hand back a [`SignalEnvelope`] and the caller carries it however
//! it likes. Nothing here knows how the remote is reached.
//!
//! Vanilla ICE: candidates ride inside the SDP, so gathering must complete
//! *before* the envelope goes out. There is no trickle message to add them
//! later.

use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    RtcConfiguration, RtcDataChannel, RtcDataChannelEvent, RtcIceConnectionState,
    RtcIceGatheringState, RtcPeerConnection, RtcSdpType, RtcSessionDescriptionInit,
};

use crate::{DATA_CHANNEL_LABEL, SIGNAL_VERSION, SignalEnvelope};

use super::transport::BrowserHubTransport;
use iroh_base::EndpointId;

/// How long to wait for the data channel after the answer is applied.
const CHANNEL_OPEN_DEADLINE_MS: f64 = 60_000.0;
/// How long to let gathering run before settling for the candidates in hand.
/// TURN allocate is slower than host/srflx; give it room before we freeze SDP.
const ICE_GATHERING_DEADLINE_MS: f64 = 10_000.0;
const POLL_MS: i32 = 50;

/// Public short-lived TURN credentials (elixir-webrtc Rel). Not for production
/// traffic volume — replace with a project-owned TURN when that matters.
const ELIXIR_TURN_CREDENTIALS_URL: &str =
    "https://turn.elixir-webrtc.org/?service=turn&username=agent-share";

/// One `RTCIceServer` entry (STUN or credentialed TURN).
#[derive(Debug, Clone)]
pub struct IceServer {
    pub urls: Vec<String>,
    pub username: Option<String>,
    pub credential: Option<String>,
}

/// ICE servers for the browser's own agent.
#[derive(Debug, Clone)]
pub struct IceServers(pub Vec<IceServer>);

impl Default for IceServers {
    fn default() -> Self {
        Self(vec![IceServer {
            urls: vec![
                "stun:stun.l.google.com:19302".to_owned(),
                "stun:stun.cloudflare.com:3478".to_owned(),
            ],
            username: None,
            credential: None,
        }])
    }
}

impl IceServers {
    #[must_use]
    pub fn host_only() -> Self {
        Self(Vec::new())
    }

    /// STUN defaults plus a short-lived public TURN relay when the fetch works.
    ///
    /// Browser↔browser on the same NAT often cannot use mDNS host candidates
    /// (macOS Local Network) or hairpin on `srflx`. TURN is the path that still
    /// connects. A failed fetch leaves STUN-only — same as before.
    pub async fn with_turn_fallback() -> Self {
        let mut servers = Self::default();
        match fetch_elixir_turn().await {
            Ok(turn) => {
                web_sys::console::log_1(&JsValue::from_str(&format!(
                    "[agent-share webrtc] TURN ready ({})",
                    turn.urls.join(", ")
                )));
                servers.0.push(turn);
            }
            Err(error) => {
                web_sys::console::warn_1(&JsValue::from_str(&format!(
                    "[agent-share webrtc] TURN credentials unavailable; STUN-only: {error:?}"
                )));
            }
        }
        servers
    }

    fn to_configuration(&self) -> RtcConfiguration {
        let config = RtcConfiguration::new();
        let servers = js_sys::Array::new();
        for server in &self.0 {
            let entry = js_sys::Object::new();
            let urls = js_sys::Array::new();
            for url in &server.urls {
                urls.push(&JsValue::from_str(url));
            }
            let _ = js_sys::Reflect::set(&entry, &JsValue::from_str("urls"), &urls);
            if let Some(username) = &server.username {
                let _ = js_sys::Reflect::set(
                    &entry,
                    &JsValue::from_str("username"),
                    &JsValue::from_str(username),
                );
            }
            if let Some(credential) = &server.credential {
                let _ = js_sys::Reflect::set(
                    &entry,
                    &JsValue::from_str("credential"),
                    &JsValue::from_str(credential),
                );
            }
            servers.push(&entry);
        }
        config.set_ice_servers(&servers);
        config
    }
}

#[derive(serde::Deserialize)]
struct ElixirTurnResponse {
    username: String,
    password: String,
    uris: Vec<String>,
}

async fn fetch_elixir_turn() -> Result<IceServer, JsValue> {
    let window = web_sys::window().ok_or_else(|| JsValue::from_str("no Window"))?;
    let opts = web_sys::RequestInit::new();
    opts.set_method("POST");
    let request = web_sys::Request::new_with_str_and_init(ELIXIR_TURN_CREDENTIALS_URL, &opts)
        .map_err(|error| js_err("TURN request", error))?;
    let response = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|error| js_err("TURN fetch", error))?;
    let response: web_sys::Response = response
        .dyn_into()
        .map_err(|_| JsValue::from_str("TURN fetch: not a Response"))?;
    if !response.ok() {
        return Err(JsValue::from_str(&format!(
            "TURN credentials HTTP {}",
            response.status()
        )));
    }
    let text = JsFuture::from(
        response
            .text()
            .map_err(|error| js_err("TURN response text", error))?,
    )
    .await
    .map_err(|error| js_err("TURN response body", error))?;
    let text = text
        .as_string()
        .ok_or_else(|| JsValue::from_str("TURN response was not a string"))?;
    let parsed: ElixirTurnResponse =
        serde_json::from_str(&text).map_err(|error| any_err("TURN credentials JSON", error))?;
    if parsed.uris.is_empty() {
        return Err(JsValue::from_str("TURN credentials had no uris"));
    }
    Ok(IceServer {
        urls: parsed.uris,
        username: Some(parsed.username),
        credential: Some(parsed.password),
    })
}

/// A negotiated browser session attached into a [`BrowserHubTransport`].
pub struct BrowserSession {
    pub remote: EndpointId,
}

impl std::fmt::Debug for BrowserSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BrowserSession")
            .field("remote", &self.remote)
            .finish()
    }
}

/// Offerer state between producing the offer and applying the answer.
pub struct PendingOffer {
    peer_connection: RtcPeerConnection,
    data_channel: RtcDataChannel,
    callbacks: Vec<JsValue>,
}

impl std::fmt::Debug for PendingOffer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PendingOffer")
            .finish_non_exhaustive()
    }
}

/// Answerer state between sending the answer and the channel opening.
pub struct PendingAnswer {
    peer_connection: RtcPeerConnection,
    /// Filled by `ondatachannel` when the remote opens the channel.
    channel_rx: futures::channel::oneshot::Receiver<RtcDataChannel>,
    callbacks: Vec<JsValue>,
}

impl std::fmt::Debug for PendingAnswer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PendingAnswer")
            .finish_non_exhaustive()
    }
}

/// Start a negotiation as the offerer.
///
/// # Errors
/// The peer connection cannot be created, SDP generation fails, or ICE
/// gathers no candidates.
pub async fn offer(
    local: EndpointId,
    ice: &IceServers,
) -> Result<(PendingOffer, SignalEnvelope), JsValue> {
    let peer_connection = RtcPeerConnection::new_with_configuration(&ice.to_configuration())
        .map_err(|error| js_err("RTCPeerConnection", error))?;
    let data_channel = peer_connection.create_data_channel(DATA_CHANNEL_LABEL);
    data_channel.set_binary_type(web_sys::RtcDataChannelType::Arraybuffer);
    // Do not install onmessage yet — `complete` attaches first so the hub
    // handler is live before the channel opens (avoids dropping QUIC Initials).

    let sdp_offer = JsFuture::from(peer_connection.create_offer())
        .await
        .map_err(|error| js_err("createOffer", error))?;
    let offer_init: RtcSessionDescriptionInit = sdp_offer.unchecked_into();
    JsFuture::from(peer_connection.set_local_description(&offer_init))
        .await
        .map_err(|error| js_err("setLocalDescription", error))?;
    wait_ice_complete(&peer_connection).await;
    let local_sdp = peer_connection
        .local_description()
        .ok_or_else(|| JsValue::from_str("no local description after gathering"))?
        .sdp();
    require_candidates("offer", &local_sdp)?;

    let envelope = SignalEnvelope::Offer {
        version: SIGNAL_VERSION,
        endpoint_id: local.to_string(),
        sdp: local_sdp,
    };
    Ok((
        PendingOffer {
            peer_connection,
            data_channel,
            callbacks: Vec::new(),
        },
        envelope,
    ))
}

impl PendingOffer {
    /// Apply the remote answer, wait for the channel, attach into `hub`.
    ///
    /// # Errors
    /// A non-answer envelope, a bad SDP, the channel never opening, or attach.
    pub async fn complete(
        self,
        hub: &BrowserHubTransport,
        answer: &SignalEnvelope,
    ) -> Result<BrowserSession, JsValue> {
        let Self {
            peer_connection,
            data_channel,
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

        // Attach before Open so inbound QUIC Initials are not lost between
        // the channel opening and the onmessage handler being installed.
        let pc = peer_connection.clone();
        hub.attach(remote, peer_connection, data_channel.clone(), callbacks)
            .map_err(|error| JsValue::from_str(&error))?;
        wait_channel_open(&data_channel, &pc).await?;
        Ok(BrowserSession { remote })
    }
}

/// Answer a remote offer.
///
/// # Errors
/// A non-offer envelope, SDP / peer-connection setup fails, or ICE gathers
/// no candidates.
pub async fn answer(
    local: EndpointId,
    offer: &SignalEnvelope,
    ice: &IceServers,
) -> Result<(PendingAnswer, SignalEnvelope), JsValue> {
    let SignalEnvelope::Offer { sdp, .. } = offer else {
        return Err(JsValue::from_str("expected an offer envelope"));
    };

    let peer_connection = RtcPeerConnection::new_with_configuration(&ice.to_configuration())
        .map_err(|error| js_err("RTCPeerConnection", error))?;

    let (channel_tx, channel_rx) = futures::channel::oneshot::channel::<RtcDataChannel>();
    let channel_tx = std::cell::RefCell::new(Some(channel_tx));
    let ondatachannel = Closure::<dyn FnMut(RtcDataChannelEvent)>::new(
        move |event: RtcDataChannelEvent| {
            let channel = event.channel();
            channel.set_binary_type(web_sys::RtcDataChannelType::Arraybuffer);
            if let Some(tx) = channel_tx.borrow_mut().take() {
                let _ = tx.send(channel);
            }
        },
    );
    peer_connection.set_ondatachannel(Some(ondatachannel.as_ref().unchecked_ref()));

    let offer_init = RtcSessionDescriptionInit::new(RtcSdpType::Offer);
    offer_init.set_sdp(sdp);
    JsFuture::from(peer_connection.set_remote_description(&offer_init))
        .await
        .map_err(|error| js_err("setRemoteDescription", error))?;

    let sdp_answer = JsFuture::from(peer_connection.create_answer())
        .await
        .map_err(|error| js_err("createAnswer", error))?;
    let answer_init: RtcSessionDescriptionInit = sdp_answer.unchecked_into();
    JsFuture::from(peer_connection.set_local_description(&answer_init))
        .await
        .map_err(|error| js_err("setLocalDescription", error))?;
    wait_ice_complete(&peer_connection).await;
    let local_sdp = peer_connection
        .local_description()
        .ok_or_else(|| JsValue::from_str("no local description after gathering"))?
        .sdp();
    require_candidates("answer", &local_sdp)?;

    let envelope = SignalEnvelope::Answer {
        version: SIGNAL_VERSION,
        endpoint_id: local.to_string(),
        sdp: local_sdp,
    };
    Ok((
        PendingAnswer {
            peer_connection,
            channel_rx,
            callbacks: vec![ondatachannel.into_js_value()],
        },
        envelope,
    ))
}

impl PendingAnswer {
    /// Wait for the offerer's data channel, then attach into `hub`.
    ///
    /// `remote` is the authenticated peer id (prefer `connection.remote_id()`
    /// over the envelope claim).
    ///
    /// # Errors
    /// The channel never arrives or attach fails.
    pub async fn complete(
        self,
        hub: &BrowserHubTransport,
        remote: EndpointId,
    ) -> Result<BrowserSession, JsValue> {
        let Self {
            peer_connection,
            channel_rx,
            callbacks,
        } = self;

        let data_channel = match futures::future::select(
            channel_rx,
            Box::pin(async {
                sleep_ms(CHANNEL_OPEN_DEADLINE_MS as i32).await;
            }),
        )
        .await
        {
            futures::future::Either::Left((Ok(channel), _)) => channel,
            futures::future::Either::Left((Err(_), _)) => {
                return Err(JsValue::from_str("data channel sender dropped"));
            }
            futures::future::Either::Right((_, _)) => {
                return Err(JsValue::from_str("timed out waiting for ondatachannel"));
            }
        };

        // Attach before Open — same race as the offerer path: the peer may
        // dial the mount ALPN the instant its channel is open.
        let pc = peer_connection.clone();
        hub.attach(remote, peer_connection, data_channel.clone(), callbacks)
            .map_err(|error| JsValue::from_str(&error))?;
        wait_channel_open(&data_channel, &pc).await?;
        Ok(BrowserSession { remote })
    }
}

/// Log local/remote SDP snippets when a dial fails after envelopes were swapped.
pub fn log_signal_sdps(role: &str, local: &SignalEnvelope, remote: &SignalEnvelope) {
    let local_sdp = envelope_sdp(local).unwrap_or("");
    let remote_sdp = envelope_sdp(remote).unwrap_or("");
    web_sys::console::log_1(&JsValue::from_str(&format!(
        "[agent-share webrtc] {role} local candidates: {}",
        format_candidate_counts(local_sdp)
    )));
    web_sys::console::log_1(&JsValue::from_str(&format!(
        "[agent-share webrtc] {role} remote candidates: {}",
        format_candidate_counts(remote_sdp)
    )));
    web_sys::console::log_1(&JsValue::from_str(&format!(
        "[agent-share webrtc] {role} local SDP:\n{local_sdp}"
    )));
    web_sys::console::log_1(&JsValue::from_str(&format!(
        "[agent-share webrtc] {role} remote SDP:\n{remote_sdp}"
    )));
}

fn envelope_sdp(envelope: &SignalEnvelope) -> Option<&str> {
    match envelope {
        SignalEnvelope::Offer { sdp, .. } | SignalEnvelope::Answer { sdp, .. } => Some(sdp),
        SignalEnvelope::Error { .. } => None,
    }
}

fn js_err(context: &str, error: JsValue) -> JsValue {
    JsValue::from_str(&format!("{context}: {error:?}"))
}

fn any_err(context: &str, error: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&format!("{context}: {error}"))
}

async fn wait_ice_complete(peer_connection: &RtcPeerConnection) {
    let deadline = now_ms() + ICE_GATHERING_DEADLINE_MS;
    loop {
        if peer_connection.ice_gathering_state() == RtcIceGatheringState::Complete {
            return;
        }
        // Always proceed after the deadline — previously we looped forever when
        // gathering stalled with zero `a=candidate` lines. Callers then check
        // for candidates and hard-fail if none landed.
        if now_ms() >= deadline {
            return;
        }
        sleep_ms(POLL_MS).await;
    }
}

fn now_ms() -> f64 {
    js_sys::Date::now()
}

/// Count ICE candidates in `sdp` and refuse a candidate-less envelope.
fn require_candidates(role: &str, sdp: &str) -> Result<(), JsValue> {
    let (host, mdns, srflx, relay, other) = count_candidates(sdp);
    let total = host + mdns + srflx + relay + other;
    web_sys::console::log_1(&JsValue::from_str(&format!(
        "[agent-share webrtc] {role} ICE candidates: host={host} mdns={mdns} srflx={srflx} relay={relay} other={other}"
    )));
    if total == 0 {
        return Err(JsValue::from_str(
            "browser gathered no ICE candidates — WebRTC/UDP blocked (Local Network permission, VPN, or policy)",
        ));
    }
    Ok(())
}

fn count_candidates(sdp: &str) -> (usize, usize, usize, usize, usize) {
    let mut host = 0;
    let mut mdns = 0;
    let mut srflx = 0;
    let mut relay = 0;
    let mut other = 0;
    for line in sdp.lines() {
        if !line.starts_with("a=candidate:") {
            continue;
        }
        if line.contains(".local") {
            mdns += 1;
        } else if line.contains(" typ host") {
            host += 1;
        } else if line.contains(" typ srflx") {
            srflx += 1;
        } else if line.contains(" typ relay") {
            relay += 1;
        } else {
            other += 1;
        }
    }
    (host, mdns, srflx, relay, other)
}

fn format_candidate_counts(sdp: &str) -> String {
    let (host, mdns, srflx, relay, other) = count_candidates(sdp);
    format!("host={host} mdns={mdns} srflx={srflx} relay={relay} other={other}")
}

fn ice_state_label(state: RtcIceConnectionState) -> &'static str {
    match state {
        RtcIceConnectionState::New => "new",
        RtcIceConnectionState::Checking => "checking",
        RtcIceConnectionState::Connected => "connected",
        RtcIceConnectionState::Completed => "completed",
        RtcIceConnectionState::Failed => "failed",
        RtcIceConnectionState::Disconnected => "disconnected",
        RtcIceConnectionState::Closed => "closed",
        _ => "unknown",
    }
}

fn gather_state_label(state: RtcIceGatheringState) -> &'static str {
    match state {
        RtcIceGatheringState::New => "new",
        RtcIceGatheringState::Gathering => "gathering",
        RtcIceGatheringState::Complete => "complete",
        _ => "unknown",
    }
}

fn channel_state_label(state: web_sys::RtcDataChannelState) -> &'static str {
    match state {
        web_sys::RtcDataChannelState::Connecting => "connecting",
        web_sys::RtcDataChannelState::Open => "open",
        web_sys::RtcDataChannelState::Closing => "closing",
        web_sys::RtcDataChannelState::Closed => "closed",
        _ => "unknown",
    }
}

async fn wait_channel_open(
    data_channel: &RtcDataChannel,
    peer_connection: &RtcPeerConnection,
) -> Result<(), JsValue> {
    let deadline = now_ms() + CHANNEL_OPEN_DEADLINE_MS;
    loop {
        let ice = peer_connection.ice_connection_state();
        if ice == RtcIceConnectionState::Failed {
            return Err(JsValue::from_str(
                "ICE failed — no route between the peers (host/mDNS blocked and TURN did not connect)",
            ));
        }
        match data_channel.ready_state() {
            web_sys::RtcDataChannelState::Open => return Ok(()),
            web_sys::RtcDataChannelState::Closing | web_sys::RtcDataChannelState::Closed => {
                return Err(JsValue::from_str(&format!(
                    "data channel closed during setup (ready_state={}, ice_connection_state={}, ice_gathering_state={})",
                    channel_state_label(data_channel.ready_state()),
                    ice_state_label(ice),
                    gather_state_label(peer_connection.ice_gathering_state()),
                )));
            }
            _ => {}
        }
        if now_ms() >= deadline {
            return Err(JsValue::from_str(&format!(
                "data channel never opened (ready_state={}, ice_connection_state={}, ice_gathering_state={})",
                channel_state_label(data_channel.ready_state()),
                ice_state_label(ice),
                gather_state_label(peer_connection.ice_gathering_state()),
            )));
        }
        sleep_ms(POLL_MS).await;
    }
}

async fn sleep_ms(millis: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        if let Some(window) = web_sys::window() {
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, millis);
        } else {
            // Avoid hanging forever outside a Window (e.g. some worker contexts).
            let _ = resolve.call0(&JsValue::NULL);
        }
    });
    let _ = JsFuture::from(promise).await;
}
