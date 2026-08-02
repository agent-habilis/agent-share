//! The share's mesh: everyone holding the link, as peers of each other.
//!
//! A ticket carries only the producer's address, so before this two consumers
//! of one share were strangers — measurably so: two browser tabs on the same
//! share each held exactly one connection, to the producer, and none to each
//! other. Joining a mesh derived from the ticket's own secret makes them peers,
//! with no ticket format change and nothing new for a user to pass around.
//!
//! Best-effort by construction. The mesh is *additional* to the mount protocol,
//! never a precondition for it: if it cannot reach a relay, or the engine
//! refuses the id, the share still serves and the file transfer is unaffected.
//! Everything here degrades to "no peer counts" rather than to a broken share.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use agent_habilis_mesh::embed::{
    AppClass, EventLoopState, HandlerCtx, InboundApp, NodeApp, NodeDriver, SilentSink,
};
use agent_habilis_mesh::net::TransportOpts;
use agent_habilis_mesh::protocol::{Message, Nickname};
use agent_habilis_mesh::runtime::{
    InjectedEndpoint, JoinParams, Node, Resolved, SetupParams, derive_topic_mesh,
};
use agent_share_proto::framing::SECRET_LEN;
use agent_share_proto::mesh_key::share_mesh_key;
use anyhow::{Context, Result};

/// How many peers this node negotiates direct sessions with. Matches the
/// engine's own ceiling and the browser peer's.
const MAX_DIRECT_PEERS: usize = 16;

/// Presence only: this node joins the mesh so its peers can see each other and
/// hold direct sessions. File bytes ride the mount protocol, not gossip, so
/// there is deliberately no application payload here.
struct ShareDriver;

#[agent_habilis_mesh::async_trait]
impl NodeApp for ShareDriver {
    fn classify(&self, _message: &Message) -> AppClass {
        AppClass {
            loggable: false,
            beat: true,
            valid: true,
            chained: false,
            sealed: false,
        }
    }

    async fn on_app_frame(
        &mut self,
        _frame: InboundApp<'_>,
        _state: &mut EventLoopState,
        _ctx: &HandlerCtx<'_>,
    ) -> bool {
        false
    }
}

#[agent_habilis_mesh::async_trait]
impl NodeDriver for ShareDriver {
    type Session = ();
    type Http = ();
    type Ipc = serde_json::Value;
}

/// A live membership in a share's mesh.
pub(crate) struct ShareMesh {
    mesh_id: String,
    /// Held, not used: dropping every clone aborts the accept task, and this
    /// Router is now the only thing accepting the share's own ALPNs.
    _router: iroh::protocol::Router,
    live: Arc<AtomicUsize>,
    webrtc: fofoca_iroh_webrtc_transport::WebRtcHandle,
    node: Option<Node<ShareDriver>>,
}

impl ShareMesh {
    /// The mesh this share landed on. Deterministic from the ticket secret, so
    /// every holder of the link computes the same one.
    pub(crate) fn mesh_id(&self) -> &str {
        &self.mesh_id
    }

    /// Members on the roster, including us.
    pub(crate) fn peers_gossip(&self) -> usize {
        self.live.load(Ordering::Relaxed)
    }

    /// Peers we hold a direct `WebRTC` data channel with.
    pub(crate) fn peers_direct(&self) -> usize {
        self.webrtc.transport().session_count()
    }

    /// Report the peer counts on a line of their own whenever they change.
    ///
    /// Append-on-change rather than a redrawn status line: the CLI has no
    /// cursor-movement anywhere, and the bench consumer already sets this
    /// precedent. Quiet until the first peer arrives, so a solo share prints
    /// nothing extra.
    ///
    /// **Silent in `--output json`.** That stream is exactly one line — the
    /// mount command — and `tests/e2e_cli_webrtc.rs` and `tests/mount.rs`
    /// scrape it. Adding to it would break them, and rightly so.
    pub(crate) fn spawn_report(&self, json: bool) {
        if json {
            return;
        }
        let live = Arc::clone(&self.live);
        let webrtc = self.webrtc.clone();
        tokio::spawn(async move {
            let mut last = None;
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                // `live` counts self; the header elsewhere says "on gossip", so
                // report other peers to match what a user would count.
                let gossip = live.load(Ordering::Relaxed).saturating_sub(1);
                let direct = webrtc.transport().session_count();
                let now = (gossip, direct);
                if last == Some(now) || now == (0, 0) {
                    continue;
                }
                last = Some(now);
                crate::util::output::status(
                    "Peers",
                    &format!("{gossip} on mesh · {direct} direct"),
                );
            }
        });
    }

    /// Broadcast `Left` and wind the node down, so peers drop us now rather
    /// than on a silence timeout. Idempotent.
    pub(crate) async fn leave(mut self) {
        if let Some(node) = self.node.take() {
            if let Err(error) = node.leave().await {
                tracing::debug!(%error, "leaving the share mesh failed");
            }
        }
    }
}

/// Join the mesh this share's secret derives, and return a handle to it.
///
/// # Errors
/// The derived id is unusable, or the node cannot bind an endpoint / reach a
/// relay. Callers treat this as non-fatal: the share serves either way.
pub(crate) async fn join(
    secret: &[u8; SECRET_LEN],
    shared: InjectedEndpoint,
    protocols: Vec<(Vec<u8>, Box<dyn iroh::protocol::DynProtocolHandler>)>,
) -> Result<ShareMesh> {
    // Hash first, then derive: the engine carries the topic *string* into its
    // state file and user-facing lines, so handing it the bearer secret would
    // print the secret. See `share_mesh_key`.
    let key = share_mesh_key(secret);
    let mesh = derive_topic_mesh(&key).context("deriving the share's mesh")?;

    let Resolved { kind, author, .. } = JoinParams {
        target: mesh
            .to_string()
            .parse()
            .context("parsing the share mesh id")?,
        nickname: Some(Nickname::random()),
        password: None,
    }
    .resolve()
    .context("resolving the share mesh join")?;

    let live = Arc::new(AtomicUsize::new(0));
    let config = agent_habilis_mesh::runtime::setup_mesh(
        kind,
        SetupParams {
            author,
            max_peers: MAX_DIRECT_PEERS,
            // No state file and no control socket: this membership belongs to
            // one `serve` process, not to a daemon something else drives.
            runtime_base: None,
            state_file: None,
            sink: Arc::new(SilentSink),
            // One endpoint for the share and the mesh, so this process has a
            // single identity. Two identities meant a viewer counted this
            // machine twice — once for the mount session, once for the mesh.
            endpoint: Some(shared),
            // The Router owns accept() now; the share's ALPNs ride along.
            protocols,
            transports: TransportOpts::default(),
            multihop: false,
            per_peer_gate: None,
            cohost: None,
            live_count: Some(Arc::clone(&live)),
        },
    )
    .await
    .context("setting up the share mesh")?;

    let mesh_id = config.mesh_id().as_str().to_owned();
    let webrtc = config.webrtc_handle();
    let router = config.router();
    // `handle_signals: false` is load-bearing, not a default. Registering
    // tokio's signal handlers suppresses the OS default-terminate for the
    // *whole process, permanently* — `serve` owns its own ctrl-c, and a mesh
    // membership must not take that away from it.
    let node = Node::spawn(config, ShareDriver, None, false);
    Ok(ShareMesh {
        mesh_id,
        _router: router,
        live,
        webrtc,
        node: Some(node),
    })
}
