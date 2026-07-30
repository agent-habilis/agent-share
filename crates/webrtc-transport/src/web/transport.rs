use std::io;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use futures::StreamExt as _;
use futures::channel::mpsc;
use iroh::EndpointId;
use iroh::endpoint::transports::{
    CustomEndpoint, CustomSender, CustomTransport, RecvInfo, Transmit,
};
use iroh_base::CustomAddr;

use crate::custom_addr;
use n0_watcher::Watchable;

/// Bound on queued outbound datagrams toward the data channel.
const OUT_QUEUE: usize = 256;
/// Bound on inbound datagrams from the data channel toward QUIC.
pub(crate) const IN_QUEUE: usize = 512;

/// The browser side of the WebRTC lane: a single session (this browser ↔
/// the daemon it signaled with). The trait impls hold only `Send` channel
/// halves; the `!Send` `RtcDataChannel` lives in `lib.rs` closures and a
/// `spawn_local` pump on the same thread.
pub struct BrowserRtcTransport {
    remote: CustomAddr,
    local_addrs: Watchable<Vec<CustomAddr>>,
    out_tx: mpsc::Sender<Vec<u8>>,
    in_rx: Mutex<Option<mpsc::Receiver<Vec<u8>>>>,
}

impl BrowserRtcTransport {
    /// Returns the transport plus the outbound receiver the data-channel
    /// pump drains. Inbound datagrams go through `in_tx` (the `onmessage`
    /// closure's half).
    pub fn new(
        local: EndpointId,
        remote: EndpointId,
        in_rx: mpsc::Receiver<Vec<u8>>,
    ) -> (Arc<Self>, mpsc::Receiver<Vec<u8>>) {
        let (out_tx, out_rx) = mpsc::channel(OUT_QUEUE);
        let transport = Arc::new(Self {
            remote: custom_addr(remote),
            local_addrs: Watchable::new(vec![custom_addr(local)]),
            out_tx,
            in_rx: Mutex::new(Some(in_rx)),
        });
        (transport, out_rx)
    }
}

impl std::fmt::Debug for BrowserRtcTransport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BrowserRtcTransport")
            .field("remote", &self.remote)
            .finish_non_exhaustive()
    }
}

impl CustomTransport for BrowserRtcTransport {
    fn bind(&self) -> io::Result<Box<dyn CustomEndpoint>> {
        let receiver = self
            .in_rx
            .lock()
            .expect("inbound receiver mutex poisoned")
            .take()
            .ok_or_else(|| io::Error::other("transport is already bound"))?;
        Ok(Box::new(BrowserEndpoint {
            remote: self.remote.clone(),
            local_addrs: self.local_addrs.clone(),
            receiver,
            out_tx: self.out_tx.clone(),
        }))
    }
}

struct BrowserEndpoint {
    remote: CustomAddr,
    local_addrs: Watchable<Vec<CustomAddr>>,
    receiver: mpsc::Receiver<Vec<u8>>,
    out_tx: mpsc::Sender<Vec<u8>>,
}

impl std::fmt::Debug for BrowserEndpoint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BrowserEndpoint")
            .finish_non_exhaustive()
    }
}

impl CustomEndpoint for BrowserEndpoint {
    fn watch_local_addrs(&self) -> n0_watcher::Direct<Vec<CustomAddr>> {
        self.local_addrs.watch()
    }

    fn create_sender(&self) -> Arc<dyn CustomSender> {
        Arc::new(BrowserSender {
            remote: self.remote.clone(),
            out_tx: self.out_tx.clone(),
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
                    return Poll::Ready(Err(io::Error::other("data channel closed")));
                }
                Poll::Ready(Some(payload)) => {
                    if bufs[0].len() < payload.len() {
                        // UDP-like semantics: oversized datagrams are lost.
                        continue;
                    }
                    bufs[0][..payload.len()].copy_from_slice(&payload);
                    metas[0].len = payload.len();
                    metas[0].stride = payload.len();
                    recv_infos[0] = RecvInfo::new(self.remote.clone(), None);
                    return Poll::Ready(Ok(1));
                }
            }
        }
    }
}

struct BrowserSender {
    remote: CustomAddr,
    out_tx: mpsc::Sender<Vec<u8>>,
}

impl std::fmt::Debug for BrowserSender {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BrowserSender")
            .finish_non_exhaustive()
    }
}

impl CustomSender for BrowserSender {
    fn is_valid_send_addr(&self, addr: &CustomAddr) -> bool {
        *addr == self.remote
    }

    fn poll_send(
        &self,
        _cx: &mut Context<'_>,
        dst: &CustomAddr,
        _src: Option<&CustomAddr>,
        transmit: &Transmit<'_>,
    ) -> Poll<io::Result<()>> {
        if *dst != self.remote {
            return Poll::Ready(Err(io::Error::from(io::ErrorKind::NotConnected)));
        }
        let chunk_size = transmit
            .segment_size
            .unwrap_or_else(|| transmit.contents.len().max(1));
        let mut out_tx = self.out_tx.clone();
        for chunk in transmit.contents.chunks(chunk_size) {
            // Drop on a full queue: QUIC above retransmits, and returning
            // `Pending` would stall the whole send loop.
            let _ = out_tx.try_send(chunk.to_vec());
        }
        Poll::Ready(Ok(()))
    }
}
