//! The `WebRTC` lane: how a peer that cannot reach us over IP still gets a
//! direct connection.
//!
//! Two connections, not one, and the split is load-bearing. iroh only fans a
//! connect's Initial out to candidate paths **while the remote has no selected
//! path**, so a live connection cannot be upgraded onto a newly attached
//! transport in place. So:
//!
//! 1. A short-lived connection on [`WEBRTC_SIGNAL_ALPN`] carries one JSEP
//!    envelope each way, then closes. Any transport will do — over the relay is
//!    the interesting case, because that is the browser's only way to reach a
//!    producer behind NAT.
//! 2. The consumer then opens a **fresh** connection to [`MOUNT_ALPN`] against
//!    an address containing *only* the `WebRTC` custom addr. No selected path,
//!    so the Initial fans out over the data channel.
//!
//! The relay is a rendezvous, not a transport: it carries the SDP exchange and
//! never a byte of file data. When ICE fails there is no second data path — the
//! dial fails loudly rather than quietly relaying.

use anyhow::{Context, Result};
use iroh::endpoint::Connection;
use iroh::{Endpoint, EndpointAddr, EndpointId, TransportAddr};
use webrtc_transport::{
    IceConfig, MAX_ENVELOPE_BYTES, SignalEnvelope, WebRtcHandle, answer_with, custom_addr,
    offer_with,
};

pub(crate) use agent_share_proto::framing::WEBRTC_SIGNAL_ALPN;

/// How long to let a JSEP negotiation run before giving up.
///
/// Generous because it covers STUN gathering on both sides plus the
/// DTLS/SCTP handshake, and a false timeout costs the whole connection.
const JSEP_DEADLINE: std::time::Duration = std::time::Duration::from_secs(20);

/// Serve one inbound signalling connection: read the offer, answer it, attach
/// the resulting session to the transport.
///
/// The envelope's `endpoint_id` is **ignored** in favour of
/// `connection.remote_id()`. Over an authenticated carrier the TLS-proven
/// identity is the real one, and trusting the claim instead would let a peer
/// attach a session under someone else's name.
///
/// # Errors
/// The stream carries no readable envelope, the SDP is unusable, or the
/// negotiation does not complete before [`JSEP_DEADLINE`].
pub(crate) async fn serve_signal(
    conn: &Connection,
    local: EndpointId,
    handle: &WebRtcHandle,
    ice: &IceConfig,
) -> Result<()> {
    let remote = conn.remote_id();
    let (mut send, mut recv) = conn.accept_bi().await.context("accept signal stream")?;
    let raw = recv
        .read_to_end(MAX_ENVELOPE_BYTES)
        .await
        .context("read signal offer")?;
    let offer: SignalEnvelope = serde_json::from_slice(&raw).context("parse signal offer")?;

    let (pending, answer) = answer_with(local, &offer, ice)
        .await
        .context("build WebRTC answer")?;
    send.write_all(&serde_json::to_vec(&answer)?)
        .await
        .context("send signal answer")?;
    send.finish().context("finish signal stream")?;

    // Boxed: the pending session carries the whole sans-io str0m state, which
    // is large enough to be worth keeping off this future's stack.
    let session = Box::pin(pending.complete(JSEP_DEADLINE))
        .await
        .context("complete WebRTC answer")?;
    handle.attach(remote, session).context("attach session")?;
    tracing::debug!(%remote, "webrtc lane attached (answerer)");
    Ok(())
}

/// Negotiate a `WebRTC` session with `producer` and return an address that
/// reaches it over the data channel and nothing else.
///
/// The returned address deliberately carries only the custom addr: handing
/// back one that also listed IP or relay paths would let the mount connection
/// pick those instead, which is exactly what this lane exists to avoid.
///
/// # Errors
/// The signalling dial fails, the producer refuses, or the negotiation does
/// not complete before [`JSEP_DEADLINE`].
pub(crate) async fn dial_webrtc(
    endpoint: &Endpoint,
    producer: EndpointAddr,
    handle: &WebRtcHandle,
    ice: &IceConfig,
) -> Result<EndpointAddr> {
    let remote = producer.id;
    let local = endpoint.id();

    let conn = endpoint
        .connect(producer, WEBRTC_SIGNAL_ALPN)
        .await
        .context("dial the WebRTC signal ALPN")?;
    let (mut send, mut recv) = conn.open_bi().await.context("open signal stream")?;

    let (pending, offer) = offer_with(local, ice).await.context("build WebRTC offer")?;
    send.write_all(&serde_json::to_vec(&offer)?)
        .await
        .context("send signal offer")?;
    send.finish().context("finish signal stream")?;

    let raw = recv
        .read_to_end(MAX_ENVELOPE_BYTES)
        .await
        .context("read signal answer")?;
    let answer: SignalEnvelope = serde_json::from_slice(&raw).context("parse signal answer")?;

    let session = Box::pin(pending.complete(&answer, JSEP_DEADLINE))
        .await
        .context("complete WebRTC offer")?;
    handle.attach(remote, session).context("attach session")?;

    // Signalling is done; the data rides its own connection.
    conn.close(0u32.into(), b"jsep done");
    tracing::debug!(%remote, "webrtc lane attached (offerer)");

    Ok(EndpointAddr::from_parts(
        remote,
        [TransportAddr::Custom(custom_addr(remote))],
    ))
}
