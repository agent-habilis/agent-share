//! A mesh peer, in a browser tab.
//!
//! This is the same engine the CLI runs — `agent-habilis-mesh` with its `host`
//! feature off — so a tab is a first-class member rather than a client of one.
//! It creates or joins a mesh, appears on every other member's roster, and
//! negotiates direct `WebRTC` data channels with them, CLI peers included.
//!
//! Two numbers come back out, and they describe different layers:
//!
//! - `peers_gossip` — the mesh roster: everyone we know is a member, however
//!   they are reached. Includes self, so a lone peer reads 1.
//! - `peers_direct` — live `WebRTC` sessions, i.e. peers we hold a direct data
//!   channel with. Always ≤ the roster, and the two converge once every peer
//!   has been negotiated with.
//!
//! Discovery needs no lookup service, which is what makes this work in a tab at
//! all: the mesh id carries a seed, the seed derives a rendezvous identity, and
//! every peer pre-registers that identity at a known relay rung. A browser has
//! no mDNS and no DHT; it does not need them.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use agent_habilis_mesh::embed::{
    AppClass, EventLoopState, HandlerCtx, InboundApp, NodeApp, NodeDriver, SelfWriteGate,
    SilentSink,
};
use agent_habilis_mesh::net::TransportOpts;
use agent_habilis_mesh::ops::{StateMergeParams, broadcast_state_merge};
use agent_habilis_mesh::protocol::{
    Channel, DirectorySelection, JoinTarget, LookupOpts, MeshConfig, MeshName, Message, Nickname,
};
use agent_habilis_mesh::runtime::{
    CreateParams, InjectedEndpoint, JoinParams, Node, Resolved, SetupParams,
    derive_topic_mesh_with, setup_mesh,
};
use agent_share_proto::PeerCard;
use agent_share_proto::framing::SECRET_LEN;
use agent_share_proto::mesh_key::share_mesh_key;
use wasm_bindgen::prelude::*;

/// Parts of a meta peer card known before the endpoint id exists.
///
/// Built by the TypeScript consumer (runtime, version, role, …). Wasm only
/// publishes what JS passes — it does not sniff `navigator.userAgent`.
#[derive(Debug, Clone)]
pub(crate) struct CardParts {
    pub version: String,
    pub runtime: String,
    pub transport: String,
    pub role: Option<String>,
}

impl CardParts {
    fn into_card(self, endpoint: String) -> PeerCard {
        PeerCard::new(
            endpoint,
            self.version,
            self.runtime,
            self.transport,
            self.role,
        )
    }
}

/// The manifest fingerprint on our own card, shared with [`MeshPeer`].
///
/// A cell rather than a field on [`CardParts`] because the browser cannot know
/// it at join: `join_share` runs from the client constructor, before any
/// manifest has been fetched. Without this the card would advertise no tree for
/// the tab's whole life. The native peer carries the same split.
pub(crate) type SharedTree = Arc<Mutex<Option<String>>>;

/// Which manifest slots this tab can serve, as `agent_share_proto::serving`
/// encodes them. Shared for the same reason as [`SharedTree`].
pub(crate) type SharedServing = Arc<Mutex<Option<String>>>;

/// What the outside world can ask the share driver to do. See the native
/// `ShareRequest`; the two are deliberately the same shape.
pub(crate) enum ShareRequest {
    /// Re-publish our meta card, picking up whatever [`SharedTree`] now holds.
    RepublishCard,
}

/// Fallback when JS omits a card (lab / older callers). No UA sniffing.
pub(crate) fn default_card_parts(
    default_transport: &str,
    default_role: Option<String>,
) -> CardParts {
    CardParts {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        runtime: "browser".to_owned(),
        transport: default_transport.to_owned(),
        role: default_role,
    }
}

/// Parse `{ version, runtime, transport?, role? }` from JS.
pub(crate) fn parse_card_parts(
    value: &JsValue,
    default_transport: &str,
    default_role: Option<String>,
) -> Result<CardParts, JsValue> {
    let version = js_sys::Reflect::get(value, &JsValue::from_str("version"))
        .ok()
        .and_then(|v| v.as_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| JsValue::from_str("peer card.version must be a non-empty string"))?;
    let runtime = js_sys::Reflect::get(value, &JsValue::from_str("runtime"))
        .ok()
        .and_then(|v| v.as_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| JsValue::from_str("peer card.runtime must be a non-empty string"))?;
    let transport = js_sys::Reflect::get(value, &JsValue::from_str("transport"))
        .ok()
        .and_then(|v| v.as_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default_transport.to_owned());
    let role = js_sys::Reflect::get(value, &JsValue::from_str("role"))
        .ok()
        .and_then(|v| v.as_string())
        .filter(|s| !s.is_empty())
        .or(default_role);
    Ok(CardParts {
        version,
        runtime,
        transport,
        role,
    })
}

/// The most direct sessions a tab will hold.
///
/// The engine's own constant, not a copy: this is both the number the tab
/// enforces and the denominator its header renders, and when they were separate
/// literals the header could show `18/16`.
use agent_habilis_mesh::net::MAX_DIRECT_PEERS;

/// Meta per-peer gate: only `<nick>` may write `/peers/<nick>/card`.
/// Must match on every share-mesh replica (genesis identity).
fn share_card_gate() -> SelfWriteGate {
    SelfWriteGate {
        map: "peers".to_owned(),
        field: "card".to_owned(),
    }
}

/// A share's lookups, as the engine spells them.
///
/// Structurally identical types in two crates, converted by hand in both leaf
/// crates that see both. `agent-share-proto` stays wasm-clean and
/// `iroh-base`-only, so it must not depend on the engine just to spare these
/// ten lines; the CLI carries the same ones.
fn mesh_lookups(
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

/// Endpoint id → peer card learned from meta `/peers/<nick>/card`.
type ClientBook = Arc<Mutex<HashMap<String, PeerCard>>>;

/// Share-mesh driver: presence plus mesh/app metadata on the meta card.
struct ShareMeshDriver {
    parts: CardParts,
    tree: SharedTree,
    serving: SharedServing,
    book: ClientBook,
}

impl ShareMeshDriver {
    fn new(parts: CardParts, tree: SharedTree, serving: SharedServing, book: ClientBook) -> Self {
        Self {
            parts,
            tree,
            serving,
            book,
        }
    }

    /// Publish mesh/app identity onto `/peers/<nick>/card` (meta channel).
    async fn publish_card(&self, state: &mut EventLoopState, ctx: &HandlerCtx<'_>) {
        let card = self
            .parts
            .clone()
            .into_card(ctx.endpoint.id().to_string())
            .with_tree(self.tree.lock().ok().and_then(|tree| tree.clone()))
            .with_serving(
                self.serving
                    .lock()
                    .ok()
                    .and_then(|serving| serving.clone()),
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
            // Swallowed before — silent failure left other tabs unable to name this peer.
            web_sys::console::warn_1(&JsValue::from_str(&format!(
                "share meta peer card publish failed: {error}"
            )));
        }
        self.refresh_book(state);
    }

    /// Rebuild the endpoint → card map from the live meta document.
    ///
    /// Includes peers that have left: nothing deletes a departed peer's CRDT
    /// entry, and a tab being closed cannot — see `ShareClient::leave_mesh`.
    /// Known defect, shared with the native peer; see `cards_from_meta` there
    /// for why the obvious roster filter was reverted.
    fn refresh_book(&self, state: &EventLoopState) {
        let doc = state.doc(Channel::Meta).to_json();
        let mut next = HashMap::new();
        if let Some(peers) = doc.get("peers").and_then(|value| value.as_object()) {
            for peer in peers.values() {
                let Some(card_value) = peer.get("card") else {
                    continue;
                };
                let Some(card) = PeerCard::from_card_value(card_value) else {
                    continue;
                };
                next.insert(card.endpoint.clone(), card);
            }
        }
        if let Ok(mut book) = self.book.lock() {
            *book = next;
        }
    }
}

#[agent_habilis_mesh::async_trait]
impl NodeApp for ShareMeshDriver {
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

    fn on_meta_applied(
        &mut self,
        _author: &Nickname,
        state: &mut EventLoopState,
        _ctx: &HandlerCtx<'_>,
    ) {
        self.refresh_book(state);
    }



    async fn on_meshed(&mut self, state: &mut EventLoopState, ctx: &HandlerCtx<'_>) {
        self.publish_card(state, ctx).await;
    }

    async fn on_peer_left(
        &mut self,
        _nickname: &Nickname,
        state: &mut EventLoopState,
        _ctx: &HandlerCtx<'_>,
    ) {
        self.refresh_book(state);
    }
}

#[agent_habilis_mesh::async_trait]
impl NodeDriver for ShareMeshDriver {
    type Session = ShareRequest;
    type Http = ();
    type Ipc = serde_json::Value;

    async fn handle_session(
        &mut self,
        req: ShareRequest,
        state: &mut EventLoopState,
        ctx: &HandlerCtx<'_>,
    ) -> bool {
        match req {
            ShareRequest::RepublishCard => {
                self.publish_card(state, ctx).await;
                true
            }
        }
    }

    async fn on_startup(&mut self, state: &mut EventLoopState, ctx: &HandlerCtx<'_>) {
        self.publish_card(state, ctx).await;
    }

}

/// A live mesh membership held by this tab.
#[wasm_bindgen]
pub struct MeshPeer {
    mesh_id: String,
    nickname: String,
    /// Roster size, mirrored out of the event loop on every membership change
    /// and on its periodic refresh. Lock-free, so the UI can poll it per frame
    /// without a request/response hop into the loop.
    live: Arc<AtomicUsize>,
    hub: Arc<fofoca_iroh_webrtc_transport::BrowserHubTransport>,
    /// Peer cards from meta `/peers/<nick>/card`, keyed by endpoint id.
    clients: ClientBook,
    /// The manifest fingerprint on our card. See [`SharedTree`].
    tree: SharedTree,
    /// Which slots this tab advertises. See [`SharedServing`].
    serving: SharedServing,
    /// Behind a `RefCell` so [`MeshPeer::leave`] can take `&self` — see the
    /// note there on why a `self`-by-value method is a trap through
    /// wasm-bindgen.
    node: RefCell<Option<Node<ShareMeshDriver>>>,
}

#[wasm_bindgen]
impl MeshPeer {
    /// Mint a new mesh and join it. The returned peer's `mesh_id` is what other
    /// peers — browser or CLI — pass to [`MeshPeer::join`].
    ///
    /// # Errors
    /// Endpoint bind failure, or no reachable relay.
    pub async fn create(
        transport: Option<String>,
        card: Option<JsValue>,
    ) -> Result<MeshPeer, JsValue> {
        console_error_panic_hook::set_once();
        let transports = parse_transport(transport.as_deref())?;
        let resolved = CreateParams {
            name: MeshName::random(),
            nickname: None,
            // The relay ladder is the load-bearing leg: it is the only one a tab
            // has, and the rendezvous homes on it. mDNS and DHT are simply
            // absent off a host, so asking for them costs nothing and buys
            // nothing here — but a CLI peer on the same mesh does use them.
            config: MeshConfig {
                lookups: LookupOpts::public_preset(),
                password: None,
                issuer_pubkey: None,
            },
            advertise: DirectorySelection::Unset,
            password: None,
            invite_only: false,
        }
        .resolve()
        .map_err(|error| err("resolve create params", &error))?;
        let parts = match card.as_ref() {
            Some(value) => parse_card_parts(value, "webrtc", None)?,
            None => default_card_parts("webrtc", None),
        };
        spawn_peer(resolved, transports, None, parts).await
    }

    /// Join an existing mesh by its id.
    ///
    /// # Errors
    /// Unparseable id, endpoint bind failure, or no reachable relay.
    pub async fn join(
        mesh_id: String,
        transport: Option<String>,
        card: Option<JsValue>,
    ) -> Result<MeshPeer, JsValue> {
        console_error_panic_hook::set_once();
        let transports = parse_transport(transport.as_deref())?;
        let target = mesh_id
            .trim()
            .parse::<JoinTarget>()
            .map_err(|error| err("parse mesh id", &error))?;
        let resolved = JoinParams {
            target,
            nickname: None,
            password: None,
        }
        .resolve()
        .map_err(|error| err("resolve join params", &error))?;
        let parts = match card.as_ref() {
            Some(value) => parse_card_parts(value, "webrtc", None)?,
            None => default_card_parts("webrtc", None),
        };
        spawn_peer(resolved, transports, None, parts).await
    }

    /// Join the mesh a *share* belongs to, derived from its ticket secret.
    ///
    /// Not exposed to JS: a caller holding the ticket already gets this for
    /// free from `ShareClient`/`ShareProducer`, and handing out a
    /// secret-taking constructor would invite passing the bearer token around
    /// by hand.
    ///
    /// Hashes before deriving for the same reason the CLI does — the engine
    /// carries the topic string into user-facing surfaces, so it must not be
    /// the secret. Both ends call `share_mesh_key`, which is why they agree.
    /// Join a share's mesh on an endpoint the caller already owns, registering
    /// the caller's ALPNs on the Router that comes with it.
    ///
    /// The producer's path. Unlike a viewer, a producer *serves* protocols, and
    /// iroh permits one accept loop per endpoint — so its ALPNs must ride the
    /// mesh's Router rather than a loop of its own.
    pub(crate) async fn join_share_with(
        secret: &[u8; SECRET_LEN],
        lookups: &agent_share_proto::lookup::LookupOpts,
        endpoint: iroh::Endpoint,
        webrtc: fofoca_iroh_webrtc_transport::WebRtcHandle,
        protocols: Vec<(Vec<u8>, Box<dyn iroh::protocol::DynProtocolHandler>)>,
        card: CardParts,
    ) -> Result<MeshPeer, JsValue> {
        let resolved = resolve_share(secret, lookups)?;
        spawn_peer_inner(
            resolved,
            TransportOpts::default(),
            Some(InjectedEndpoint { endpoint, webrtc }),
            protocols,
            card,
        )
        .await
    }

    pub(crate) async fn join_share(
        secret: &[u8; SECRET_LEN],
        lookups: &agent_share_proto::lookup::LookupOpts,
        shared: Option<(iroh::Endpoint, fofoca_iroh_webrtc_transport::WebRtcHandle)>,
        card: CardParts,
    ) -> Result<MeshPeer, JsValue> {
        let resolved = resolve_share(secret, lookups)?;
        spawn_peer(resolved, TransportOpts::default(), shared, card).await
    }

    #[wasm_bindgen(getter)]
    pub fn mesh_id(&self) -> String {
        self.mesh_id.clone()
    }

    #[wasm_bindgen(getter)]
    pub fn nickname(&self) -> String {
        self.nickname.clone()
    }

    /// The WebRTC hub this peer negotiates mesh sessions on.
    pub(crate) fn hub(&self) -> &Arc<fofoca_iroh_webrtc_transport::BrowserHubTransport> {
        &self.hub
    }

    /// Meta peer card for `endpoint_id`, if published.
    pub(crate) fn card_for(&self, endpoint_id: &str) -> Option<PeerCard> {
        self.clients
            .lock()
            .ok()
            .and_then(|book| book.get(endpoint_id).cloned())
    }

    /// Publish the manifest fingerprint this tab is on.
    ///
    /// The browser cannot supply this at join — `join_share` runs from the
    /// client constructor, before any manifest exists — so without calling this
    /// the tab advertises no tree for its whole life and is never a candidate
    /// source for anyone.
    ///
    /// Idempotent by value, and that is the debounce: a card rewrite is a CRDT
    /// merge broadcast to the whole mesh, and callers fire far more often than
    /// the value changes. The native peer's `set_tree` is the same function.
    pub(crate) async fn set_tree(&self, fingerprint: String) {
        {
            // Scoped: never hold a std `Mutex` across the await below.
            let Ok(mut current) = self.tree.lock() else {
                return;
            };
            if current.as_deref() == Some(fingerprint.as_str()) {
                return;
            }
            *current = Some(fingerprint);
        }
        // Cloned out rather than borrowed across the await: `node` lives behind
        // a `RefCell`, and holding that borrow over a yield point is how a
        // browser task panics on a re-entrant borrow.
        let sender = self.node.borrow().as_ref().map(Node::sender);
        let Some(sender) = sender else {
            return;
        };
        if let Err(error) = sender.send(ShareRequest::RepublishCard).await {
            web_sys::console::debug_1(&JsValue::from_str(&format!(
                "[share] republishing the card failed: {error}"
            )));
        }
    }

    /// Publish which manifest slots this tab can serve.
    ///
    /// The same debounce-by-value as [`Self::set_tree`], and for the same
    /// reason: this rides a CRDT merge broadcast to the whole mesh, and a sync
    /// that fetched nothing new must not cost everyone a gossip round.
    ///
    /// `None` clears the field, which reads as *cannot vouch* rather than
    /// *holds nothing* — the distinction `serving` is built on.
    pub(crate) async fn set_serving(&self, encoded: Option<String>) {
        {
            // Scoped: never hold a std `Mutex` across the await below.
            let Ok(mut current) = self.serving.lock() else {
                return;
            };
            if *current == encoded {
                return;
            }
            *current = encoded;
        }
        let sender = self.node.borrow().as_ref().map(Node::sender);
        let Some(sender) = sender else {
            return;
        };
        if let Err(error) = sender.send(ShareRequest::RepublishCard).await {
            web_sys::console::debug_1(&JsValue::from_str(&format!(
                "[share] republishing the card failed: {error}"
            )));
        }
    }

    /// All meta peer cards currently known (gossip roster advertise).
    pub(crate) fn known_cards(&self) -> Vec<PeerCard> {
        self.clients
            .lock()
            .ok()
            .map(|book| book.values().cloned().collect())
            .unwrap_or_default()
    }

    /// Members on the gossip roster, including self — so a lone peer reads 1.
    #[wasm_bindgen(getter)]
    pub fn peers_gossip(&self) -> u32 {
        u32::try_from(self.live.load(Ordering::Relaxed)).unwrap_or(u32::MAX)
    }

    /// Peers we hold a live `WebRTC` data channel with. Excludes self.
    #[wasm_bindgen(getter)]
    pub fn peers_direct(&self) -> u32 {
        u32::try_from(self.hub.session_count()).unwrap_or(u32::MAX)
    }

    /// The direct-session ceiling this peer negotiates up to.
    #[wasm_bindgen(getter)]
    pub fn max_direct(&self) -> u32 {
        u32::try_from(MAX_DIRECT_PEERS).unwrap_or(u32::MAX)
    }

    /// Leave the mesh: broadcast `Left` so peers drop us now rather than on a
    /// silence timeout, then wind the loop down. Idempotent.
    ///
    /// `&self`, not `self`: a `self`-by-value method compiles to a
    /// `__destroy_into_raw()` in the wasm-bindgen glue, so a second call from
    /// JS passes a null pointer and traps the whole wasm instance. The lab
    /// page calls this directly.
    ///
    /// # Errors
    /// The event loop returned an error while shutting down.
    pub async fn leave(&self) -> Result<(), JsValue> {
        // Scoped: the `RefMut` must not be alive across the await below.
        let node = self.node.borrow_mut().take();
        if let Some(node) = node {
            node.leave()
                .await
                .map_err(|error| err("leave mesh", &error))?;
        }
        Ok(())
    }
}

/// Stand the node up from resolved params. Shared by create and join — the only
/// difference between them is the `SetupKind` that lands here.
/// The mesh a share's secret derives, resolved for joining.
///
/// Hashes before deriving: the engine carries the topic string into its state
/// file and user-facing lines, so it must not be the bearer secret.
fn resolve_share(
    secret: &[u8; SECRET_LEN],
    lookups: &agent_share_proto::lookup::LookupOpts,
) -> Result<Resolved, JsValue> {
    // The share's own reach, from the ticket both ends read — not the public
    // preset. See `derive_topic_mesh_with`.
    let mesh = derive_topic_mesh_with(&share_mesh_key(secret), mesh_lookups(lookups))
        .map_err(|error| err("derive the share mesh", &error))?;
    let target = mesh
        .to_string()
        .parse::<JoinTarget>()
        .map_err(|error| err("parse the share mesh id", &error))?;
    JoinParams {
        target,
        nickname: None,
        password: None,
    }
    .resolve()
    .map_err(|error| err("resolve the share mesh join", &error))
}

/// `None` / `"dynamic"` ⇒ everything available; `"webrtc"` ⇒ WebRTC-only data
/// plane. A tab has no IP transports either way, so this mostly matters for
/// symmetry with the CLI flag — and so a browser-side test can *state* the
/// contract it is asserting rather than relying on the target implying it.
fn parse_transport(mode: Option<&str>) -> Result<TransportOpts, JsValue> {
    match mode.map(str::trim).filter(|mode| !mode.is_empty()) {
        None | Some("dynamic") => Ok(TransportOpts::default()),
        Some("webrtc") => Ok(TransportOpts::webrtc_only()),
        Some(other) => Err(JsValue::from_str(&format!(
            "unknown transport {other:?}; expected `webrtc` or `dynamic`"
        ))),
    }
}

async fn spawn_peer(
    resolved: Resolved,
    transports: TransportOpts,
    shared: Option<(iroh::Endpoint, fofoca_iroh_webrtc_transport::WebRtcHandle)>,
    card: CardParts,
) -> Result<MeshPeer, JsValue> {
    let injected = shared.map(|(endpoint, webrtc)| InjectedEndpoint { endpoint, webrtc });
    spawn_peer_inner(resolved, transports, injected, Vec::new(), card).await
}

async fn spawn_peer_inner(
    resolved: Resolved,
    transports: TransportOpts,
    injected: Option<InjectedEndpoint>,
    protocols: Vec<(Vec<u8>, Box<dyn iroh::protocol::DynProtocolHandler>)>,
    card: CardParts,
) -> Result<MeshPeer, JsValue> {
    let Resolved { kind, author, .. } = resolved;
    let live = Arc::new(AtomicUsize::new(0));
    let clients: ClientBook = Arc::new(Mutex::new(HashMap::new()));
    let config = setup_mesh(
        kind,
        SetupParams {
            author: author.clone(),
            max_peers: MAX_DIRECT_PEERS,
            // Share the mount's endpoint when there is one, so this tab has a
            // single identity and a single hub. That is what lets the mount
            // session show up in `peers_direct` immediately, instead of the
            // count sitting at 0 while files are visibly loading.
            endpoint: injected,
            protocols,
            transports,
            // A tab writes no files and binds no socket. Both are already
            // optional on the engine, so this is configuration rather than a
            // special case.
            runtime_base: None,
            state_file: None,
            sink: Arc::new(SilentSink),
            multihop: false,
            // Gate meta so only `<nick>` may write `/peers/<nick>/card`.
            per_peer_gate: Some(share_card_gate()),
            cohost: None,
            live_count: Some(Arc::clone(&live)),
        },
    )
    .await
    .map_err(|error| err("set up mesh", &error))?;

    let mesh_id = config.mesh_id().as_str().to_owned();
    let hub = config.webrtc_handle().transport();
    // Seed our own card so Info does not wait on meta sync to self.
    let own = card.clone().into_card(hub.local_id().to_string());
    if let Ok(mut book) = clients.lock() {
        book.insert(own.endpoint.clone(), own);
    }
    // `handle_signals: false` — there are no process signals in a tab, and the
    // engine's signal registration is host-only anyway.
    let tree: SharedTree = Arc::new(Mutex::new(None));
    let serving: SharedServing = Arc::new(Mutex::new(None));
    let driver = ShareMeshDriver::new(
        card,
        Arc::clone(&tree),
        Arc::clone(&serving),
        Arc::clone(&clients),
    );
    let node = Node::spawn(config, driver, None, false);
    Ok(MeshPeer {
        mesh_id,
        nickname: author.to_string(),
        live,
        hub,
        clients,
        tree,
        serving,
        node: RefCell::new(Some(node)),
    })
}

fn err(context: &str, error: &impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&format!("{context}: {error}"))
}
