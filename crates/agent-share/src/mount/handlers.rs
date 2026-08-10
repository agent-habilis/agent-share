//! The share's two protocols, as `ProtocolHandler`s on the mesh's Router.
//!
//! `serve` used to own `endpoint.accept()` and branch on
//! `conn.alpn()`. It cannot any more, because the share and the mesh now use
//! **one** endpoint, and iroh permits exactly one accept loop on an endpoint:
//!
//! - `Router::spawn` calls `Endpoint::set_alpns`, which its own documentation
//!   says *overrides* the ALPN list — so ALPNs set at build time silently
//!   vanish the moment the mesh Router spawns.
//! - Two accept loops are two consumers of one queue, so each inbound
//!   connection goes to whichever loop wins the race.
//!
//! So the Router owns accept, and these register with it. The ALPN branch that
//! used to live in `accept_one` is now the Router's dispatch, which is where it
//! belonged all along.
//!
//! One endpoint is not a tidiness win. Two endpoints meant this process
//! appeared on the network as two unrelated identities, so a viewer counted the
//! same machine twice — once for the mount session, once for the mesh session.

use agent_share_proto::auth::ShareAuth;
use fofoca::iroh::EndpointId;
use fofoca::iroh::endpoint::Connection;
use fofoca::iroh::protocol::{AcceptError, ProtocolHandler};
use fofoca_iroh_webrtc_transport::{IceConfig, WebRtcHandle};

/// Serves `MOUNT_ALPN`: one long-lived connection, one request per bi-stream.
///
/// Generic over what answers, so the same handler serves a producer reading
/// through to files and a mount seeding from its chunk store. The browser has
/// its own copy of this and only this — its sources are `!Send` and need a
/// `SendWrapper`, which would panic under a multi-threaded runtime, so the
/// handler is the one piece of the serving path that cannot be shared. The
/// protocol it drives is [`agent_share_mount::serve_stream`], and that is.
#[derive(Debug, Clone)]
pub(crate) struct MountHandler {
    /// What an inbound request has to present, and how to refuse one that
    /// does not. Redacts itself in `Debug`, which this struct derives.
    auth: ShareAuth,
    source: super::source::NativeSource,
}

impl MountHandler {
    pub(crate) fn new(auth: ShareAuth, source: super::source::NativeSource) -> Self {
        Self { auth, source }
    }
}

impl ProtocolHandler for MountHandler {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        // Held for the connection's life, exactly as the old accept loop's
        // spawned task was. Errors are the peer going away, which is routine.
        if let Err(error) =
            super::produce::serve_established(conn, self.auth, self.source.clone()).await
        {
            tracing::debug!(%error, "mount connection ended");
        }
        Ok(())
    }
}

/// Serves the share's `WEBRTC_SIGNAL_ALPN`: one JSEP exchange, then done.
///
/// Distinct from the mesh's own signal ALPN and deliberately still separate —
/// they attach into the same hub now, but the share's dialer speaks its own
/// protocol and collapsing the two is a wire change, not a refactor.
#[derive(Debug, Clone)]
pub(crate) struct SignalHandler {
    local: EndpointId,
    webrtc: WebRtcHandle,
    ice: IceConfig,
}

impl SignalHandler {
    pub(crate) fn new(local: EndpointId, webrtc: WebRtcHandle, ice: IceConfig) -> Self {
        Self { local, webrtc, ice }
    }
}

impl ProtocolHandler for SignalHandler {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        if let Err(error) =
            super::webrtc::serve_signal(&conn, self.local, &self.webrtc, &self.ice).await
        {
            tracing::debug!(%error, "webrtc signalling ended");
        }
        Ok(())
    }
}
