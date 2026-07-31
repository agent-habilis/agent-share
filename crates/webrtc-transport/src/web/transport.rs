//! Multi-session browser `WebRTC` transport.
//!
//! The consumer and the producer share one shape: a hub that owns zero or more
//! data channels, keyed by remote [`EndpointId`]. The consumer attaches one
//! session after offering; the producer attaches each peer that signals in.

use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use futures::SinkExt as _;
use futures::StreamExt as _;
use futures::channel::mpsc;
use iroh::EndpointId;
use iroh::endpoint::transports::{
    CustomEndpoint, CustomSender, CustomTransport, RecvInfo, Transmit,
};
use iroh_base::CustomAddr;
use n0_watcher::Watchable;
use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::*;
use web_sys::{MessageEvent, RtcDataChannel, RtcPeerConnection};

use crate::custom_addr;

/// Bound on queued outbound datagrams per session.
const OUT_QUEUE: usize = 256;
/// Bound on inbound datagrams from every session toward QUIC.
pub(crate) const IN_QUEUE: usize = 512;
/// Stop queueing into the channel above this much buffered data.
const BUFFER_CAP: u32 = 1 << 20;

pub(crate) struct InboundPacket {
    pub(crate) from: EndpointId,
    pub(crate) payload: Vec<u8>,
}

struct SessionHandle {
    out_tx: mpsc::Sender<Vec<u8>>,
    /// Keeps the peer connection and data channel alive.
    _keepalive: SessionKeepalive,
}

/// Opaque hold on browser handles that must outlive the QUIC path.
struct SessionKeepalive {
    _peer_connection: RtcPeerConnection,
    _data_channel: RtcDataChannel,
    _callbacks: Vec<JsValue>,
}

type SessionMap = Arc<Mutex<HashMap<EndpointId, SessionHandle>>>;

/// Factory for the browser `WebRTC` datagram lane of one iroh endpoint.
pub struct BrowserHubTransport {
    local_id: EndpointId,
    sessions: SessionMap,
    inbound_tx: mpsc::Sender<InboundPacket>,
    inbound_rx: Mutex<Option<mpsc::Receiver<InboundPacket>>>,
    local_addrs: Watchable<Vec<CustomAddr>>,
}

impl BrowserHubTransport {
    #[must_use]
    pub fn new(local_id: EndpointId) -> Arc<Self> {
        let (inbound_tx, inbound_rx) = mpsc::channel(IN_QUEUE);
        Arc::new(Self {
            local_id,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            inbound_tx,
            inbound_rx: Mutex::new(Some(inbound_rx)),
            local_addrs: Watchable::new(vec![custom_addr(local_id)]),
        })
    }

    #[must_use]
    pub fn local_id(&self) -> EndpointId {
        self.local_id
    }

    /// Adopt an open data channel for `remote` and start its pumps.
    ///
    /// # Errors
    /// A live session for `remote` already exists.
    pub fn attach(
        &self,
        remote: EndpointId,
        peer_connection: RtcPeerConnection,
        data_channel: RtcDataChannel,
        mut callbacks: Vec<JsValue>,
    ) -> Result<(), String> {
        let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(OUT_QUEUE);
        let (mut in_tx_session, mut in_rx_session) = mpsc::channel::<Vec<u8>>(IN_QUEUE);

        let onmessage = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
            let Some(bytes) = message_bytes(&event) else {
                return;
            };
            let _ = in_tx_session.try_send(bytes);
        });
        data_channel.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
        callbacks.push(onmessage.into_js_value());

        {
            let mut inbound_tx = self.inbound_tx.clone();
            wasm_bindgen_futures::spawn_local(async move {
                while let Some(payload) = in_rx_session.next().await {
                    // Await rather than try_send: a single full queue used to
                    // tear down the whole pump and kill the WebRTC lane.
                    if inbound_tx
                        .send(InboundPacket {
                            from: remote,
                            payload,
                        })
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            });
        }

        {
            let data_channel = data_channel.clone();
            wasm_bindgen_futures::spawn_local(async move {
                while let Some(datagram) = out_rx.next().await {
                    if data_channel.buffered_amount() < BUFFER_CAP {
                        let _ = data_channel.send_with_u8_array(&datagram);
                    }
                }
            });
        }

        let mut sessions = self
            .sessions
            .lock()
            .expect("browser hub session map poisoned");
        if sessions.contains_key(&remote) {
            return Err(format!("a live WebRTC session for {remote} already exists"));
        }
        sessions.insert(
            remote,
            SessionHandle {
                out_tx,
                _keepalive: SessionKeepalive {
                    _peer_connection: peer_connection,
                    _data_channel: data_channel,
                    _callbacks: callbacks,
                },
            },
        );
        Ok(())
    }

    /// Tear down the session for `remote`, if any.
    pub fn detach(&self, remote: &EndpointId) -> bool {
        self.sessions
            .lock()
            .expect("browser hub session map poisoned")
            .remove(remote)
            .is_some()
    }
}

impl std::fmt::Debug for BrowserHubTransport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BrowserHubTransport")
            .field("local_id", &self.local_id)
            .field(
                "sessions",
                &self
                    .sessions
                    .lock()
                    .map(|map| map.len())
                    .unwrap_or(0),
            )
            .finish_non_exhaustive()
    }
}

impl CustomTransport for BrowserHubTransport {
    fn bind(&self) -> io::Result<Box<dyn CustomEndpoint>> {
        let receiver = self
            .inbound_rx
            .lock()
            .expect("inbound receiver mutex poisoned")
            .take()
            .ok_or_else(|| io::Error::other("BrowserHubTransport is already bound"))?;
        Ok(Box::new(BrowserHubEndpoint {
            local_addrs: self.local_addrs.clone(),
            receiver,
            sessions: Arc::clone(&self.sessions),
        }))
    }
}

struct BrowserHubEndpoint {
    local_addrs: Watchable<Vec<CustomAddr>>,
    receiver: mpsc::Receiver<InboundPacket>,
    sessions: SessionMap,
}

impl std::fmt::Debug for BrowserHubEndpoint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BrowserHubEndpoint")
            .finish_non_exhaustive()
    }
}

impl CustomEndpoint for BrowserHubEndpoint {
    fn watch_local_addrs(&self) -> n0_watcher::Direct<Vec<CustomAddr>> {
        self.local_addrs.watch()
    }

    fn create_sender(&self) -> Arc<dyn CustomSender> {
        Arc::new(BrowserHubSender {
            sessions: Arc::clone(&self.sessions),
        })
    }

    fn poll_recv(
        &mut self,
        cx: &mut Context<'_>,
        bufs: &mut [io::IoSliceMut<'_>],
        metas: &mut [noq_udp::RecvMeta],
        recv_infos: &mut [RecvInfo],
    ) -> Poll<io::Result<usize>> {
        if bufs.is_empty() || metas.is_empty() || recv_infos.is_empty() {
            return Poll::Ready(Ok(0));
        }
        loop {
            match self.receiver.poll_next_unpin(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => {
                    return Poll::Ready(Err(io::Error::other("inbound packet channel closed")));
                }
                Poll::Ready(Some(packet)) => {
                    if bufs[0].len() < packet.payload.len() {
                        continue;
                    }
                    bufs[0][..packet.payload.len()].copy_from_slice(&packet.payload);
                    metas[0].len = packet.payload.len();
                    metas[0].stride = packet.payload.len();
                    // Match the original single-session transport: leave local
                    // unset. Some iroh paths treat a Custom local addr oddly
                    // when the path was learned only from the data channel.
                    recv_infos[0] = RecvInfo::new(custom_addr(packet.from), None);
                    return Poll::Ready(Ok(1));
                }
            }
        }
    }
}

fn message_bytes(event: &MessageEvent) -> Option<Vec<u8>> {
    let data = event.data();
    if let Ok(buffer) = data.clone().dyn_into::<js_sys::ArrayBuffer>() {
        return Some(js_sys::Uint8Array::new(&buffer).to_vec());
    }
    if let Ok(array) = data.dyn_into::<js_sys::Uint8Array>() {
        return Some(array.to_vec());
    }
    None
}

struct BrowserHubSender {
    sessions: SessionMap,
}

impl std::fmt::Debug for BrowserHubSender {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BrowserHubSender")
            .finish_non_exhaustive()
    }
}

impl CustomSender for BrowserHubSender {
    fn is_valid_send_addr(&self, addr: &CustomAddr) -> bool {
        let Ok(remote) = crate::parse_custom_addr(addr) else {
            return false;
        };
        self.sessions
            .lock()
            .expect("browser hub session map poisoned")
            .contains_key(&remote)
    }

    fn poll_send(
        &self,
        _cx: &mut Context<'_>,
        dst: &CustomAddr,
        _src: Option<&CustomAddr>,
        transmit: &Transmit<'_>,
    ) -> Poll<io::Result<()>> {
        let Ok(remote) = crate::parse_custom_addr(dst) else {
            return Poll::Ready(Err(io::Error::from(io::ErrorKind::NotConnected)));
        };
        let chunk_size = transmit
            .segment_size
            .unwrap_or_else(|| transmit.contents.len().max(1));
        let sessions = self
            .sessions
            .lock()
            .expect("browser hub session map poisoned");
        let Some(handle) = sessions.get(&remote) else {
            return Poll::Ready(Err(io::Error::from(io::ErrorKind::NotConnected)));
        };
        let mut out_tx = handle.out_tx.clone();
        for chunk in transmit.contents.chunks(chunk_size) {
            let _ = out_tx.try_send(chunk.to_vec());
        }
        Poll::Ready(Ok(()))
    }
}

/// Backward-compatible name used by the consume path and older docs.
pub type BrowserRtcTransport = BrowserHubTransport;
