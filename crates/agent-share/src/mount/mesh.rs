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
    AppClass, EventLoopState, HandlerCtx, InboundApp, NodeApp, NodeDriver, SelfWriteGate,
    SilentSink,
};
use agent_habilis_mesh::net::TransportOpts;
use agent_habilis_mesh::ops::{StateMergeParams, broadcast_state_merge};
use agent_habilis_mesh::protocol::{Channel, Message, Nickname};
use agent_habilis_mesh::runtime::{
    InjectedEndpoint, JoinParams, Node, Resolved, SetupParams, derive_topic_mesh_with,
};
use agent_share_proto::PeerCard;
use agent_share_proto::framing::SECRET_LEN;
use agent_share_proto::mesh_key::share_mesh_key;
use anyhow::{Context, Result};

/// A share's lookups, as the engine spells them.
///
/// The two types are structurally identical and carry the same `RelayUrl` —
/// the workspace pins one `iroh-base` — but they belong to different crates,
/// and deliberately: `agent-share-proto` is wasm-clean and depends on
/// `iroh-base` alone, so it must not learn about the engine just to spare this
/// function. The browser peer carries the same ten lines for the same reason.
pub(crate) fn mesh_lookups(
    share: &agent_share_proto::lookup::LookupOpts,
) -> agent_habilis_mesh::protocol::LookupOpts {
    use agent_habilis_mesh::protocol::RelayChoice as MeshRelay;
    use agent_share_proto::lookup::RelayChoice as ShareRelay;
    agent_habilis_mesh::protocol::LookupOpts {
        mdns: share.mdns,
        dht: share.dht,
        relay: match &share.relay {
            ShareRelay::Disabled => MeshRelay::Disabled,
            ShareRelay::Pinned => MeshRelay::Pinned,
            ShareRelay::Custom(urls) => MeshRelay::Custom(urls.clone()),
        },
    }
}

/// How many peers this node negotiates direct sessions with.
///
/// The engine's own constant, not a copy of it. It used to be a local `16`
/// here, another in the browser peer, and the real one in the engine — so the
/// number a UI rendered and the number the engine enforced could drift apart,
/// and did.
use agent_habilis_mesh::net::MAX_DIRECT_PEERS;

/// Meta per-peer gate: only `<nick>` may write `/peers/<nick>/card`.
fn share_card_gate() -> SelfWriteGate {
    SelfWriteGate {
        map: "peers".to_owned(),
        field: "card".to_owned(),
    }
}

/// Presence plus mesh/app metadata on the meta card. File bytes ride the mount
/// protocol, not gossip.
struct ShareDriver {
    version: String,
    runtime: String,
    transport: String,
    role: Option<String>,
}

impl ShareDriver {
    fn new(version: String, runtime: String, transport: String, role: Option<String>) -> Self {
        Self {
            version,
            runtime,
            transport,
            role,
        }
    }

    async fn publish_card(&self, state: &mut EventLoopState, ctx: &HandlerCtx<'_>) {
        let card = PeerCard::new(
            ctx.endpoint.id().to_string(),
            &self.version,
            &self.runtime,
            &self.transport,
            self.role.clone(),
        );
        let merge = serde_json::json!({
            "peers": {
                ctx.author.as_str(): {
                    "card": card.to_card_value()
                }
            }
        });
        if let Err(error) = broadcast_state_merge(
            state,
            StateMergeParams {
                mesh: ctx.mesh,
                author: ctx.author,
                merge,
                sender: ctx.sender,
                sink: ctx.sink,
                channel: Channel::Meta,
                surface: false,
            },
        )
        .await
        {
            tracing::debug!(%error, "share meta client card publish failed");
        }
    }
}

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

    async fn on_meshed(&mut self, state: &mut EventLoopState, ctx: &HandlerCtx<'_>) {
        self.publish_card(state, ctx).await;
    }
}

#[agent_habilis_mesh::async_trait]
impl NodeDriver for ShareDriver {
    type Session = ();
    type Http = ();
    type Ipc = serde_json::Value;

    async fn on_startup(&mut self, state: &mut EventLoopState, ctx: &HandlerCtx<'_>) {
        self.publish_card(state, ctx).await;
    }
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
                if last == Some(now) {
                    continue;
                }
                // `last` is updated whichever way the next branch goes.
                // Skipping the update on the quiet path is how a share went
                // permanently silent after a single join/leave cycle: the
                // return to zero was dropped without recording it, so when the
                // peer came back the identical state read as a repeat and was
                // dropped too. Nothing ever printed again.
                let first_sample = last.is_none();
                last = Some(now);
                // Quiet *until* the first peer arrives, so a solo share prints
                // nothing extra — but a return to zero afterwards is real news
                // and gets a line.
                if now == (0, 0) && first_sample {
                    continue;
                }
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
        if let Some(node) = self.node.take()
            && let Err(error) = node.leave().await
        {
            tracing::debug!(%error, "leaving the share mesh failed");
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
    lookups: &agent_share_proto::lookup::LookupOpts,
    shared: InjectedEndpoint,
    protocols: Vec<(Vec<u8>, Box<dyn iroh::protocol::DynProtocolHandler>)>,
) -> Result<ShareMesh> {
    // Hash first, then derive: the engine carries the topic *string* into its
    // state file and user-facing lines, so handing it the bearer secret would
    // print the secret. See `share_mesh_key`.
    let key = share_mesh_key(secret);
    // The share's own reach, not the public preset. Producer and consumers
    // agree by construction because they read it from the same ticket, so this
    // needs no new ticket field — and a private share stops standing up a
    // public rendezvous it could never reach anyway.
    let mesh =
        derive_topic_mesh_with(&key, mesh_lookups(lookups)).context("deriving the share's mesh")?;

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
            per_peer_gate: Some(share_card_gate()),
            cohost: None,
            live_count: Some(Arc::clone(&live)),
        },
    )
    .await
    .context("setting up the share mesh")?;

    let mesh_id = config.mesh_id().as_str().to_owned();
    let webrtc = config.webrtc_handle();
    let router = config.router();
    // Native CLI peers advertise as unicast — that is the directed path they
    // take on the share mesh (iroh QUIC), distinct from a browser's webrtc/relay.
    // This join path is the producer today (`mount::produce`).
    let driver = ShareDriver::new(
        env!("CARGO_PKG_VERSION").to_owned(),
        "rust".to_owned(),
        "unicast".to_owned(),
        Some("producer".to_owned()),
    );
    // `handle_signals: false` is load-bearing, not a default. Registering
    // tokio's signal handlers suppresses the OS default-terminate for the
    // *whole process, permanently* — `serve` owns its own ctrl-c, and a mesh
    // membership must not take that away from it.
    let node = Node::spawn(config, driver, None, false);
    Ok(ShareMesh {
        mesh_id,
        _router: router,
        live,
        webrtc,
        node: Some(node),
    })
}
