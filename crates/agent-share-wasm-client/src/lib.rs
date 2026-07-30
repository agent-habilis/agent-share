//! The browser client: read a share over a `WebRTC` data channel.
//!
//! No web-specific protocol. This speaks the same `agent-share/mount/1` ALPN
//! the CLI does, over the same `webrtc-transport`, using the same
//! `agent-share-proto` wire types — the browser is a peer, not a special case.
//!
//! # The two-connection dance
//!
//! iroh only fans a connect's Initial out to candidate paths **while the
//! remote has no selected path**, so a live connection cannot be upgraded onto
//! a newly attached transport. Hence:
//!
//! 1. Dial `agent-share/webrtc-signal/1` over the relay and swap one JSEP
//!    envelope each way. The relay is a rendezvous — it carries the SDP and
//!    never a byte of file data.
//! 2. Open a **fresh** connection to `agent-share/mount/1` against an address
//!    carrying only the `WebRTC` custom addr.
//!
//! When ICE fails, so does the connection: there is no relayed data path to
//! fall back to, by design.

use std::sync::Arc;

use agent_share_proto::framing::{
    self, MAX_MANIFEST_BYTES, MOUNT_ALPN, SECRET_LEN, WEBRTC_SIGNAL_ALPN,
};
use agent_share_proto::manifest::MountManifest;
use agent_share_proto::ticket::MountTicket;
use iroh::endpoint::{Connection, presets};
use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey, TransportAddr};
use wasm_bindgen::prelude::*;
use webrtc_transport::{
    BrowserSession, IceServers, MAX_ENVELOPE_BYTES, SignalEnvelope, WebRtcHandle, browser_offer,
    custom_addr,
};

/// A connected share, ready to list and read.
#[wasm_bindgen]
pub struct ShareClient {
    connection: Connection,
    secret: [u8; SECRET_LEN],
    // Held so the data channel outlives the connection riding on it.
    _session: BrowserSession,
    _endpoint: Endpoint,
}

#[wasm_bindgen]
impl ShareClient {
    /// Decode `ticket`, negotiate a data channel with its producer, and open
    /// the mount connection over it.
    ///
    /// # Errors
    /// The ticket is malformed, the producer is unreachable, or ICE fails —
    /// the last of which is fatal here, since no relayed data path exists.
    pub async fn connect(ticket: String) -> Result<ShareClient, JsValue> {
        console_error_panic_hook::set_once();
        let ticket = MountTicket::decode(&ticket).map_err(|error| err("decode ticket", &error))?;
        let producer = ticket.addr.id;

        // One key, two endpoints. A custom transport can only be registered at
        // build time, and the transport itself does not exist until the JSEP
        // exchange has produced a session — which needs an endpoint to happen
        // over. So: a relay-only endpoint does the signalling, then a second
        // endpoint on the *same* key carries the data. The key must match,
        // because the transport advertises `custom_addr(local)` as the address
        // peers dial it on.
        let key = SecretKey::generate();
        let local = key.public();

        let signaller = Endpoint::builder(presets::Minimal)
            .secret_key(key.clone())
            .relay_mode(relay_mode(&ticket))
            .bind()
            .await
            .map_err(|error| err("bind signalling endpoint", &error))?;

        let session = negotiate(&signaller, ticket.addr.clone(), local).await?;
        // The relay's job is over — it carried the SDP and nothing else.
        signaller.close().await;

        let handle = WebRtcHandle::new(Arc::clone(&session.transport));
        let endpoint = Endpoint::builder(presets::Minimal)
            .secret_key(key)
            .relay_mode(RelayMode::Disabled)
            .add_custom_transport(handle.transport())
            .bind()
            .await
            .map_err(|error| err("bind data endpoint", &error))?;

        // Fresh connection, WebRTC-only address: no selected path, so the
        // Initial fans out over the data channel.
        let webrtc_only = EndpointAddr::from_parts(
            producer,
            [TransportAddr::Custom(custom_addr(producer))],
        );
        let connection = endpoint
            .connect(webrtc_only, MOUNT_ALPN)
            .await
            .map_err(|error| err("dial the mount ALPN over WebRTC", &error))?;

        Ok(ShareClient {
            connection,
            secret: ticket.secret,
            _session: session,
            _endpoint: endpoint,
        })
    }

    /// Which path the data is on. Always `"webrtc"` — the relay is a
    /// rendezvous only, so there is no other answer.
    #[must_use]
    #[wasm_bindgen(getter)]
    pub fn transport(&self) -> String {
        "webrtc".to_owned()
    }

    /// The whole tree, in one shot: `{ dirs: [...], files: [...] }`.
    ///
    /// One request by design — the protocol has no per-directory listing op,
    /// so navigation is instant and only *bytes* are lazy.
    ///
    /// # Errors
    /// The producer refuses the request or the manifest does not decode.
    pub async fn manifest(&self) -> Result<JsValue, JsValue> {
        let (mut send, mut recv) = self
            .connection
            .open_bi()
            .await
            .map_err(|error| err("open manifest stream", &error))?;
        send.write_all(&framing::encode_manifest_request(&self.secret))
            .await
            .map_err(|error| err("send manifest request", &error))?;
        send.finish().map_err(|error| err("finish", &error))?;

        let len = read_header(&mut recv, MAX_MANIFEST_BYTES).await?;
        let mut bytes = vec![0u8; len as usize];
        recv.read_exact(&mut bytes)
            .await
            .map_err(|error| err("read manifest", &error))?;
        let manifest =
            MountManifest::decode(&bytes).map_err(|error| err("decode manifest", &error))?;
        serde_wasm(&manifest)
    }

    /// One byte range of one file, addressed by its index in the manifest.
    ///
    /// Capped at `MAX_READ_LEN` (256 KiB) per call by the protocol; chunk
    /// larger reads yourself.
    ///
    /// # Errors
    /// A bad index, an unreadable file, or a length over the producer's cap.
    pub async fn read(&self, index: u32, offset: u64, len: u32) -> Result<Vec<u8>, JsValue> {
        let (mut send, mut recv) = self
            .connection
            .open_bi()
            .await
            .map_err(|error| err("open read stream", &error))?;
        send.write_all(&framing::encode_read_request(&self.secret, index, offset, len))
            .await
            .map_err(|error| err("send read request", &error))?;
        send.finish().map_err(|error| err("finish", &error))?;

        let got = read_header(&mut recv, len).await?;
        let mut data = vec![0u8; got as usize];
        recv.read_exact(&mut data)
            .await
            .map_err(|error| err("read body", &error))?;
        Ok(data)
    }
}

/// Swap one JSEP envelope each way over the signal ALPN, then attach.
async fn negotiate(
    endpoint: &Endpoint,
    producer: EndpointAddr,
    local: iroh_base::EndpointId,
) -> Result<BrowserSession, JsValue> {
    let conn = endpoint
        .connect(producer, WEBRTC_SIGNAL_ALPN)
        .await
        .map_err(|error| err("dial the signal ALPN", &error))?;
    let (mut send, mut recv) = conn
        .open_bi()
        .await
        .map_err(|error| err("open signal stream", &error))?;

    let (pending, offer) = browser_offer(local, &IceServers::default()).await?;
    let encoded = serde_json::to_vec(&offer).map_err(|error| err("encode offer", &error))?;
    send.write_all(&encoded)
        .await
        .map_err(|error| err("send offer", &error))?;
    send.finish().map_err(|error| err("finish", &error))?;

    let raw = recv
        .read_to_end(MAX_ENVELOPE_BYTES)
        .await
        .map_err(|error| err("read answer", &error))?;
    let answer: SignalEnvelope =
        serde_json::from_slice(&raw).map_err(|error| err("parse answer", &error))?;
    let session = pending.complete(&answer).await?;

    // Signalling is done; the relay's job ends here.
    conn.close(0u32.into(), b"jsep done");
    Ok(session)
}

/// Read the `status(1) ‖ len(u32)` prefix every response carries.
async fn read_header(
    recv: &mut iroh::endpoint::RecvStream,
    cap: u32,
) -> Result<u32, JsValue> {
    let mut prefix = [0u8; 5];
    recv.read_exact(&mut prefix)
        .await
        .map_err(|error| err("read response header", &error))?;
    framing::decode_response_header(&prefix, cap).map_err(|error| err("response", &error))
}

/// The relay ladder the ticket carries. `Disabled` on a loopback ticket, where
/// there is nothing to reach.
fn relay_mode(ticket: &MountTicket) -> RelayMode {
    use agent_share_proto::lookup::RelayChoice;
    match &ticket.lookups.relay {
        RelayChoice::Disabled => RelayMode::Disabled,
        RelayChoice::Pinned => iroh::endpoint::default_relay_mode(),
        RelayChoice::Custom(ladder) => RelayMode::custom(ladder.iter().cloned()),
    }
}

fn err(context: &str, error: &impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&format!("{context}: {error}"))
}

fn serde_wasm<T: serde::Serialize>(value: &T) -> Result<JsValue, JsValue> {
    let json = serde_json::to_string(value).map_err(|error| err("serialize", &error))?;
    js_sys::JSON::parse(&json)
}
