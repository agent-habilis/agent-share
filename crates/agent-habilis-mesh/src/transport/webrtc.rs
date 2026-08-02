//! The mesh's `WebRTC` signalling plane: how two peers that cannot reach each
//! other over IP still end up holding a direct data channel.
//!
//! The engine already carries a [`WebRtcHandle`] into `build_endpoint`
//! (`lookup::TransportHandles`), but until this module nothing ever *filled*
//! it: the transport was registered and permanently empty, because a session
//! only exists once two peers have exchanged SDP, and nothing exchanged SDP.
//! This is that exchange.
//!
//! **The relay is the rendezvous.** One short-lived connection on
//! [`MESH_WEBRTC_SIGNAL_ALPN`] carries one JSEP envelope each way and closes.
//! Any transport will do, and over the relay is the interesting case — it is a
//! browser's only way to reach a peer behind NAT, and the mesh's own bootstrap
//! already homes every member on a relay rung. No file or gossip payload ever
//! crosses it; it exists to introduce two peers to each other.
//!
//! **Why a session must exist before the peer is grafted.** iroh only fans a
//! connect's Initial out to candidate paths *while the remote has no selected
//! path*, so a live connection cannot be upgraded onto a newly attached
//! transport in place. A gossip graft that beats the JSEP round therefore pins
//! that pair to the relay for as long as the link lives. `lifecycle`'s dial
//! deferral is where the ordering is enforced; this module only provides the
//! two halves of the exchange.
//!
//! Both roles are written once and split by target only where the JSEP APIs
//! genuinely differ (str0m natively, `RTCPeerConnection` in a tab). The wire
//! format — [`SignalEnvelope`] — is shared, so a browser and a CLI peer
//! negotiate with each other without either knowing which it is talking to.

use anyhow::{Context, Result};
use fofoca_iroh_webrtc_transport::{MAX_ENVELOPE_BYTES, SignalEnvelope, WebRtcHandle};
use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh::{Endpoint, EndpointAddr, EndpointId};

use super::LOG_TARGET;

/// ALPN for the JSEP exchange. Wire-load-bearing in the same way
/// [`super::UNICAST_ALPN`] is: both ends must agree, so it moves only with a
/// deliberate protocol break.
pub(crate) const MESH_WEBRTC_SIGNAL_ALPN: &[u8] = b"habilis-mesh/webrtc-signal/1";

/// How long to let one negotiation run before giving up.
///
/// Generous because it covers candidate gathering on both sides plus the
/// DTLS/SCTP handshake, and a false timeout costs the whole session. Note the
/// browser side gathers with a vanilla-ICE budget of its own (candidates ride
/// inside the SDP; there is no trickle message), so this must comfortably
/// exceed it.
#[cfg(not(target_arch = "wasm32"))]
const JSEP_DEADLINE: std::time::Duration = std::time::Duration::from_secs(20);

/// The `ProtocolHandler` the Router runs for [`MESH_WEBRTC_SIGNAL_ALPN`]: read
/// one offer, answer it, attach the resulting session to our hub.
///
/// Holds the hub rather than a channel to the event loop, deliberately. The
/// exchange is self-contained and the loop has nothing to decide about it — and
/// routing it through the loop would put a multi-second negotiation on the one
/// task that must never block.
#[derive(Debug, Clone)]
pub(crate) struct WebRtcSignalAcceptor {
    handle: WebRtcHandle,
    local: EndpointId,
}

impl WebRtcSignalAcceptor {
    pub(crate) fn new(handle: WebRtcHandle, local: EndpointId) -> Self {
        Self { handle, local }
    }
}

impl ProtocolHandler for WebRtcSignalAcceptor {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        // The remote's identity comes from the *connection*, never from the
        // envelope. Over an authenticated carrier the TLS-proven id is the real
        // one, and trusting `SignalEnvelope::endpoint_id` instead would let any
        // peer attach a session under someone else's name — which, on a mesh
        // where sessions are keyed by peer, is impersonation rather than merely
        // a wasted negotiation.
        let remote = conn.remote_id();
        let handle = self.handle.clone();
        let local = self.local;
        // Negotiate off the accept future, for two independent reasons. It can
        // take seconds (candidate gathering, then DTLS/SCTP), and holding the
        // Router's accept task that long would serialize inbound offers. And in
        // a browser the JSEP path holds `!Send` web-sys closures, which iroh's
        // `Send` accept future cannot carry at all — `n0_future::task::spawn` is
        // `spawn_local` there, so the requirement simply does not apply.
        n0_future::task::spawn(async move {
            match answer_one(&conn, local, remote, &handle).await {
                Ok(()) => {
                    tracing::debug!(target: LOG_TARGET, %remote, "webrtc session attached (answerer)");
                }
                // A failed negotiation is normal operation, not a fault: ICE
                // fails, peers vanish mid-handshake, a NAT refuses. The peer
                // stays reachable over whatever path it already had.
                Err(error) => {
                    tracing::debug!(target: LOG_TARGET, %remote, %error, "webrtc answer failed");
                }
            }
        });
        Ok(())
    }
}

/// Read the offer off `conn`, answer it, and attach the session.
async fn answer_one(
    conn: &Connection,
    local: EndpointId,
    remote: EndpointId,
    handle: &WebRtcHandle,
) -> Result<()> {
    let (mut send, mut recv) = conn.accept_bi().await.context("accept signal stream")?;
    let raw = recv
        .read_to_end(MAX_ENVELOPE_BYTES)
        .await
        .context("read signal offer")?;
    let offer: SignalEnvelope = serde_json::from_slice(&raw).context("parse signal offer")?;

    // Order is load-bearing: put the answer on the wire *before* completing the
    // negotiation. The offerer cannot finish ICE until it has our SDP, so
    // completing first deadlocks both sides into their full gathering budget
    // and then fails — which is exactly what it did.
    let answer = build_answer(local, &offer).await?;
    send.write_all(&serde_json::to_vec(answer.envelope())?)
        .await
        .context("send signal answer")?;
    send.finish().context("finish signal stream")?;
    answer.complete(remote, handle).await
}

/// Offer a session to `peer` and attach it. The caller decides *whether* to
/// dial (see the role rule in `lifecycle`); this only performs the exchange.
///
/// # Errors
/// The signalling dial fails, the peer refuses, or the negotiation does not
/// complete before the deadline.
pub(crate) async fn dial_signal(
    endpoint: &Endpoint,
    peer: EndpointAddr,
    handle: &WebRtcHandle,
) -> Result<()> {
    let remote = peer.id;
    let local = endpoint.id();

    let conn = endpoint
        .connect(peer, MESH_WEBRTC_SIGNAL_ALPN)
        .await
        .context("dial the mesh WebRTC signal ALPN")?;
    let (mut send, mut recv) = conn.open_bi().await.context("open signal stream")?;

    let offer = build_offer(local, handle).await?;
    send.write_all(&serde_json::to_vec(offer.envelope())?)
        .await
        .context("send signal offer")?;
    send.finish().context("finish signal stream")?;

    let raw = recv
        .read_to_end(MAX_ENVELOPE_BYTES)
        .await
        .context("read signal answer")?;
    let answer: SignalEnvelope = serde_json::from_slice(&raw).context("parse signal answer")?;

    offer.with_answer(answer).complete(remote, handle).await?;
    // Signalling is done the moment the session is attached; the data rides its
    // own connections from here.
    conn.close(0u32.into(), b"jsep done");
    tracing::debug!(target: LOG_TARGET, %remote, "webrtc session attached (offerer)");
    Ok(())
}

/// The most direct peers one node will negotiate sessions with.
///
/// This caps the *session* mesh, which is dense by intent — we want every peer
/// we know about, not just our gossip neighbours. It is deliberately not the
/// same knob as HyParView's `active_view_capacity`: that one sizes the gossip
/// overlay and is fixed when the mesh is built, while this one is ours and can
/// move at runtime. Each session costs a peer connection and, in a browser, up
/// to a full ICE gathering budget — so the ceiling is real, not notional.
pub(crate) const MAX_DIRECT_PEERS: usize = 16;

/// Start a `WebRTC` negotiation with `peer`, if one is wanted and not already
/// running. Fire-and-forget: the caller does **not** wait, and the graft
/// proceeds over whatever path is available.
///
/// An earlier version of this gated the graft — held the peer out of the gossip
/// overlay until a direct session existed — on the theory that iroh will not
/// move a live connection onto a transport attached later, so a link formed
/// first stays on the relay. That reasoning is correct, but the cure was worse:
/// running it against a real CLI peer and two browser peers, the CLI reached
/// `link_len=0, meshed=false` and never joined the overlay at all. The
/// higher-id side waits to be dialled, the lower-id side only dials when it
/// sees a `PeerInfo`, and when that ordering does not line up the pair simply
/// never links. Mesh membership is the thing that must not be fragile.
///
/// So: gossip links form immediately, over the relay if that is what is
/// available, and the direct session is negotiated alongside. Connections
/// opened *after* attach — unicast, blob — take the WebRTC path. The gossip
/// link for that pair may stay relayed, which is a real cost and the reason
/// `peers_direct` and `peers_gossip` are reported separately rather than as one
/// number.
///
/// **Who offers is decided by id order** — the lower `EndpointId` dials. Both
/// sides compute the same answer with no round trip, so simultaneous mutual
/// offers (which collide on duplicate attach) cannot happen. This mirrors the
/// tie-break `lifecycle` already applies to the gossip dial itself.
pub(crate) fn negotiate_session(
    state: &mut crate::daemon::state::EventLoopState,
    ctx: &crate::daemon::ctx::HandlerCtx<'_>,
    peer: EndpointId,
    addr: EndpointAddr,
) {
    let Some(handle) = state.webrtc.clone() else {
        // No transport registered: the beacon, or a multihop peer.
        return;
    };
    if handle.transport().has_session(&peer) {
        return;
    }
    // The higher id waits to be dialled, so exactly one offer crosses per pair.
    let local = ctx.endpoint.id();
    if local > peer {
        return;
    }
    {
        let inflight = state
            .webrtc_dialing
            .lock()
            .expect("webrtc dialing set poisoned");
        if inflight.contains(&peer) {
            return;
        }
    }
    if handle.transport().session_count() >= MAX_DIRECT_PEERS {
        tracing::debug!(
            target: LOG_TARGET,
            %peer,
            cap = MAX_DIRECT_PEERS,
            "direct-peer cap reached; not negotiating"
        );
        return;
    }

    state
        .webrtc_dialing
        .lock()
        .expect("webrtc dialing set poisoned")
        .insert(peer);
    let endpoint = ctx.endpoint.clone();
    let inflight = std::sync::Arc::clone(&state.webrtc_dialing);
    n0_future::task::spawn(async move {
        let outcome = dial_signal(&endpoint, addr, &handle).await;
        // Clear the marker on *both* paths. A failed round must be retryable —
        // ICE fails for transient reasons all the time — and the next
        // `PeerInfo` re-flood is what retries it.
        inflight
            .lock()
            .expect("webrtc dialing set poisoned")
            .remove(&peer);
        if let Err(error) = outcome {
            tracing::debug!(target: LOG_TARGET, %peer, %error, "webrtc offer failed");
        }
    });
}

// ── Per-target JSEP ───────────────────────────────────────────────────────
//
// Only these two helpers differ by backend. Everything above — the ALPN, the
// envelope framing, the identity rule, the logging — is written once.

// Gated by target, not by our own `host` feature: which backend the transport
// crate compiles is decided by the target dependency table, so a
// `--no-default-features` *native* build still has str0m, not the browser hub.
#[cfg(not(target_arch = "wasm32"))]
mod backend {
    use super::{Context, EndpointId, JSEP_DEADLINE, Result, SignalEnvelope, WebRtcHandle};
    use fofoca_iroh_webrtc_transport::{
        IceConfig, PendingAnswer, PendingOffer, answer_with, offer_with,
    };

    /// An offer awaiting its answer, plus the envelope to put on the wire.
    pub(super) struct Offer {
        pending: PendingOffer,
        envelope: SignalEnvelope,
        answer: Option<SignalEnvelope>,
    }

    impl Offer {
        pub(super) fn envelope(&self) -> &SignalEnvelope {
            &self.envelope
        }

        pub(super) async fn complete(
            mut self,
            remote: EndpointId,
            handle: &WebRtcHandle,
        ) -> Result<()> {
            let answer = self.answer.take().context("answer not supplied")?;
            // Boxed: the pending session carries the whole sans-io str0m state,
            // large enough to be worth keeping off this future's stack.
            let session = Box::pin(self.pending.complete(&answer, JSEP_DEADLINE))
                .await
                .context("complete WebRTC offer")?;
            handle.attach(remote, session).context("attach session")
        }

        pub(super) fn with_answer(mut self, answer: SignalEnvelope) -> Self {
            self.answer = Some(answer);
            self
        }
    }

    pub(super) async fn build_offer(local: EndpointId, _handle: &WebRtcHandle) -> Result<Offer> {
        let ice = IceConfig::default();
        let (pending, envelope) = offer_with(local, &ice)
            .await
            .context("build WebRTC offer")?;
        Ok(Offer {
            pending,
            envelope,
            answer: None,
        })
    }

    /// An answer whose envelope is ready to send, with the negotiation still to
    /// be driven. Split so the caller can put the SDP on the wire first.
    pub(super) struct Answer {
        pending: PendingAnswer,
        envelope: SignalEnvelope,
    }

    impl Answer {
        pub(super) fn envelope(&self) -> &SignalEnvelope {
            &self.envelope
        }

        pub(super) async fn complete(
            self,
            remote: EndpointId,
            handle: &WebRtcHandle,
        ) -> Result<()> {
            let session = Box::pin(self.pending.complete(JSEP_DEADLINE))
                .await
                .context("complete WebRTC answer")?;
            handle.attach(remote, session).context("attach session")
        }
    }

    pub(super) async fn build_answer(local: EndpointId, offer: &SignalEnvelope) -> Result<Answer> {
        let ice = IceConfig::default();
        let (pending, envelope) = answer_with(local, offer, &ice)
            .await
            .context("build WebRTC answer")?;
        Ok(Answer { pending, envelope })
    }
}

#[cfg(target_arch = "wasm32")]
mod backend {
    use super::{Context, EndpointId, Result, SignalEnvelope, WebRtcHandle};
    use fofoca_iroh_webrtc_transport::{
        BrowserPendingAnswer, BrowserPendingOffer, IceServers, browser_answer, browser_offer,
    };

    pub(super) struct Offer {
        pending: BrowserPendingOffer,
        envelope: SignalEnvelope,
        answer: Option<SignalEnvelope>,
    }

    impl Offer {
        pub(super) fn envelope(&self) -> &SignalEnvelope {
            &self.envelope
        }

        pub(super) async fn complete(
            mut self,
            remote: EndpointId,
            handle: &WebRtcHandle,
        ) -> Result<()> {
            let answer = self.answer.take().context("answer not supplied")?;
            // The browser `complete` attaches into the hub itself, keyed on the
            // id the *answer envelope claims*. We hold the TLS-proven id, so
            // reject a mismatch before it can plant a session under the wrong
            // peer.
            let claimed = answer
                .claimed_endpoint()
                .context("answer carries no usable endpoint id")?;
            anyhow::ensure!(
                claimed == remote,
                "signal answer claims {claimed}, but the connection is with {remote}"
            );
            self.pending
                .complete(&handle.transport(), &answer)
                .await
                .map_err(|error| anyhow::anyhow!("complete WebRTC offer: {error:?}"))?;
            Ok(())
        }

        pub(super) fn with_answer(mut self, answer: SignalEnvelope) -> Self {
            self.answer = Some(answer);
            self
        }
    }

    pub(super) async fn build_offer(local: EndpointId, _handle: &WebRtcHandle) -> Result<Offer> {
        let ice = IceServers::with_turn_fallback().await;
        let (pending, envelope) = browser_offer(local, &ice)
            .await
            .map_err(|error| anyhow::anyhow!("build WebRTC offer: {error:?}"))?;
        Ok(Offer {
            pending,
            envelope,
            answer: None,
        })
    }

    pub(super) struct Answer {
        pending: BrowserPendingAnswer,
        envelope: SignalEnvelope,
    }

    impl Answer {
        pub(super) fn envelope(&self) -> &SignalEnvelope {
            &self.envelope
        }

        pub(super) async fn complete(
            self,
            remote: EndpointId,
            handle: &WebRtcHandle,
        ) -> Result<()> {
            // Keyed on the TLS-proven id the caller passed, not the envelope
            // claim.
            self.pending
                .complete(&handle.transport(), remote)
                .await
                .map_err(|error| anyhow::anyhow!("complete WebRTC answer: {error:?}"))?;
            Ok(())
        }
    }

    pub(super) async fn build_answer(local: EndpointId, offer: &SignalEnvelope) -> Result<Answer> {
        let ice = IceServers::with_turn_fallback().await;
        let (pending, envelope) = browser_answer(local, offer, &ice)
            .await
            .map_err(|error| anyhow::anyhow!("build WebRTC answer: {error:?}"))?;
        Ok(Answer { pending, envelope })
    }
}

use backend::{build_answer, build_offer};

/// Re-attempt negotiation with every known peer we hold no session with.
///
/// Driven by a periodic tick rather than by `PeerInfo`, because `PeerInfo` is
/// the *arrival* signal and stops re-flooding once a pair is linked. A single
/// failed round would otherwise be permanent — the pair keeps a working relay
/// link and silently never gets a direct path, which is the failure mode that
/// looks like everything is fine.
///
/// Cheap: a map walk plus a `has_session` check per peer. The per-peer in-flight
/// guard and the cap inside [`negotiate_session`] do the rest of the work.
pub(crate) fn retry_sessions(
    state: &mut crate::daemon::state::EventLoopState,
    ctx: &crate::daemon::ctx::HandlerCtx<'_>,
) {
    if state.webrtc.is_none() {
        return;
    }
    // Collected first: `negotiate_session` needs `&mut state`, so the borrow of
    // `peer_endpoints` cannot be held across the calls.
    let peers: Vec<EndpointId> = state
        .peer_endpoints
        .values()
        .copied()
        .filter(|peer| *peer != ctx.rendezvous_id)
        .collect();
    for peer in peers {
        negotiate_session(state, ctx, peer, EndpointAddr::new(peer));
    }
}
