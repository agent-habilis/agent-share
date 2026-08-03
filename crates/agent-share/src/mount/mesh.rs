//! The share's mesh: everyone holding the link, as peers of each other.
//!
//! A ticket carries only the producer's address, so before this two consumers
//! of one share were strangers — measurably so: two browser tabs on the same
//! share each held exactly one connection, to the producer, and none to each
//! other. Joining a mesh derived from the ticket's own secret makes them peers,
//! with no ticket format change and nothing new for a user to pass around.
//!
//! *Every* peer of a share joins: both browser roles, the CLI producer, and the
//! CLI consumer. A peer that stays off the mesh is invisible to it — it does not
//! appear on anyone's roster, and it counts nobody — so a single hold-out makes
//! every other peer's count wrong rather than merely incomplete. That is why
//! [`join`] takes the peer's [`Role`] instead of assuming the producer's.
//!
//! Best-effort by construction. The mesh is *additional* to the mount protocol,
//! never a precondition for it: if it cannot reach a relay, or the engine
//! refuses the id, the share still serves and the file transfer is unaffected.
//! Everything here degrades to "no peer counts" rather than to a broken share.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

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

/// Which side of the share a peer is on, as its meta card spells it.
///
/// The two strings are a vocabulary shared with the browser peer — `role` on
/// the meta card, rendered per peer by the web Info panel (`PeerRole` in
/// `web/src/peerCard`). An enum rather than a `&str` argument so a typo is a
/// compile error instead of a peer that renders as an unknown role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    /// Serves the tree: `mount::produce`.
    Producer,
    /// Reads the tree: `mount::consume`.
    Consumer,
}

impl Role {
    fn as_card_str(self) -> &'static str {
        match self {
            Self::Producer => "producer",
            Self::Consumer => "consumer",
        }
    }
}

/// Meta per-peer gate: only `<nick>` may write `/peers/<nick>/card`.
fn share_card_gate() -> SelfWriteGate {
    SelfWriteGate {
        map: "peers".to_owned(),
        field: "card".to_owned(),
    }
}

/// Every peer's published card, keyed by endpoint id.
///
/// Shared with [`ShareMesh`] rather than owned by the driver: the driver lives
/// inside the engine's event loop and is unreachable from the outside, so a
/// handle that outlives a borrow of it is the only way a caller can read the
/// roster. The browser peer carries the same split for the same reason.
type CardBook = Arc<Mutex<HashMap<String, PeerCard>>>;

/// Read every `/peers/<nick>/card` out of a meta document.
///
/// Free function rather than a method so the part with the interesting
/// behaviour — what it tolerates — is testable without standing up an engine.
///
/// **Keyed by endpoint, not by nickname.** Nicknames are random per join
/// ([`Nickname::random`] below), so the same machine that rejoins appears under
/// a new one; the endpoint id is the identity that a mount session can actually
/// be addressed by. A peer occupying two nicknames therefore collapses to one
/// entry, which is the desired reading: it is one peer.
fn cards_from_meta(doc: &serde_json::Value) -> HashMap<String, PeerCard> {
    let mut cards = HashMap::new();
    let Some(peers) = doc.get("peers").and_then(serde_json::Value::as_object) else {
        return cards;
    };
    for peer in peers.values() {
        let Some(card_value) = peer.get("card") else {
            continue;
        };
        // A card this build cannot parse is skipped, never fatal: a peer on an
        // older or newer shape must cost us that one peer, not the roster.
        let Some(card) = PeerCard::from_card_value(card_value) else {
            continue;
        };
        cards.insert(card.endpoint.clone(), card);
    }
    cards
}

/// Snapshot the roster out of a shared book.
fn cards_from_book(book: &CardBook) -> Vec<PeerCard> {
    book.lock()
        .ok()
        .map(|book| book.values().cloned().collect())
        .unwrap_or_default()
}

/// Describe the *other* peers by role, for the `Peers` status line.
///
/// The raw count already says how many peers there are; what a user actually
/// wants when a share misbehaves is whether the peer they can see is the one
/// serving the bytes. `""` when the roster says nothing useful — a count with a
/// misleading breakdown beside it is worse than a count alone, and the roster
/// legitimately lags the gossip counter: a peer is on the mesh before it has
/// published a card.
fn roles_suffix(cards: &[PeerCard], local_endpoint: &str) -> String {
    let mut producers = 0usize;
    let mut consumers = 0usize;
    for card in cards.iter().filter(|card| card.endpoint != local_endpoint) {
        match card.role.as_deref() {
            Some("producer") => producers += 1,
            Some("consumer") => consumers += 1,
            _ => {}
        }
    }
    match (producers, consumers) {
        (0, 0) => String::new(),
        (serving, 0) => format!(" ({serving} producing)"),
        (0, reading) => format!(" ({reading} reading)"),
        (serving, reading) => format!(" ({serving} producing, {reading} reading)"),
    }
}

/// Presence plus mesh/app metadata on the meta card. File bytes ride the mount
/// protocol, not gossip.
struct ShareDriver {
    version: String,
    runtime: String,
    transport: String,
    role: Option<String>,
    book: CardBook,
}

impl ShareDriver {
    fn new(
        version: String,
        runtime: String,
        transport: String,
        role: Option<String>,
        book: CardBook,
    ) -> Self {
        Self {
            version,
            runtime,
            transport,
            role,
            book,
        }
    }

    /// Rebuild the endpoint → card map from the live meta document.
    ///
    /// A full rebuild per event rather than a patch. The document is the
    /// authority and it is small — one card per peer — so re-reading it costs
    /// nothing next to the gossip round-trip that triggered it, and it cannot
    /// drift from what the CRDT actually says. `on_peer_left` needs the same
    /// path anyway, since a departure is an absence rather than an edit.
    fn refresh_book(&self, state: &EventLoopState) {
        let next = cards_from_meta(&state.doc(Channel::Meta).to_json());
        if let Ok(mut book) = self.book.lock() {
            *book = next;
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
        // Our own card is part of the roster, and publishing it is the one
        // change that never arrives as an inbound meta event.
        self.refresh_book(state);
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

    /// Someone's meta card changed — including ours, echoed back.
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

    /// A departure is an absence, not an edit, so nothing arrives on the meta
    /// channel to trigger [`Self::refresh_book`] — without this hook a peer
    /// that left would stay on the roster until the process ended.
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
    book: CardBook,
    /// Our own endpoint id, so the roster breakdown can report *other* peers
    /// and agree with the count beside it, which already excludes self.
    local_endpoint: String,
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
        let book = Arc::clone(&self.book);
        let local = self.local_endpoint.clone();
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
                    &format!(
                        "{gossip} on mesh{} · {direct} direct",
                        roles_suffix(&cards_from_book(&book), &local)
                    ),
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

/// What a peer brings to a share's mesh, and how it describes itself there.
///
/// A struct rather than six positional arguments: `protocols` and `transports`
/// are both "empty/default for one role, not the other", and two adjacent
/// defaultable arguments are exactly the shape that gets swapped silently.
pub(crate) struct JoinOpts<'a> {
    /// The ticket's bearer secret. Hashed into the mesh id, never carried on
    /// it — see [`share_mesh_key`].
    pub(crate) secret: &'a [u8; SECRET_LEN],
    /// Read off the *ticket*, never off a local binding: every peer of a share
    /// deriving its mesh from what the ticket says is what makes them agree.
    pub(crate) lookups: &'a agent_share_proto::lookup::LookupOpts,
    /// The endpoint this process already speaks the mount protocol on, so the
    /// share and the mesh are one identity rather than two peers on one host.
    pub(crate) shared: InjectedEndpoint,
    /// ALPNs to serve on the mesh's Router. The producer's two ride here
    /// because iroh permits one accept loop per endpoint; a consumer serves
    /// none and passes an empty vec.
    pub(crate) protocols: Vec<(Vec<u8>, Box<dyn iroh::protocol::DynProtocolHandler>)>,
    pub(crate) role: Role,
    /// Must match the reach the injected endpoint was built with. A consumer
    /// run under `--transport webrtc` binds with IP cleared, so leaving the
    /// mesh on the default would have it advertise paths that do not exist.
    pub(crate) transports: TransportOpts,
}

/// Join the mesh this share's secret derives, and return a handle to it.
///
/// # Errors
/// The derived id is unusable, or the node cannot bind an endpoint / reach a
/// relay. Callers treat this as non-fatal: the share serves either way.
pub(crate) async fn join(opts: JoinOpts<'_>) -> Result<ShareMesh> {
    let JoinOpts {
        secret,
        lookups,
        shared,
        protocols,
        role,
        transports,
    } = opts;
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

    // Read before `shared` is moved into the setup params below.
    let local_endpoint = shared.endpoint.id().to_string();
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
            transports,
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
    // True of both roles: a consumer reads files over the mount protocol, but
    // its *mesh* traffic rides the same unicast plane the producer's does.
    let book: CardBook = Arc::new(Mutex::new(HashMap::new()));
    let driver = ShareDriver::new(
        env!("CARGO_PKG_VERSION").to_owned(),
        "rust".to_owned(),
        "unicast".to_owned(),
        Some(role.as_card_str().to_owned()),
        Arc::clone(&book),
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
        book,
        local_endpoint,
    })
}

#[cfg(test)]
mod tests {
    use super::{Role, cards_from_book, cards_from_meta, roles_suffix};
    use agent_share_proto::PeerCard;

    /// A meta document shaped the way `publish_card` writes one.
    fn meta_with(peers: &[(&str, serde_json::Value)]) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        for (nick, card) in peers {
            map.insert(
                (*nick).to_owned(),
                serde_json::json!({ "card": card.clone() }),
            );
        }
        serde_json::json!({ "peers": serde_json::Value::Object(map) })
    }

    fn card(endpoint: &str, role: Role) -> serde_json::Value {
        PeerCard::new(
            endpoint,
            "0.1.0",
            "rust",
            "unicast",
            Some(role.as_card_str().to_owned()),
        )
        .to_card_value()
    }

    #[test]
    fn an_empty_document_yields_an_empty_roster() {
        assert!(cards_from_meta(&serde_json::json!({})).is_empty());
        assert!(cards_from_meta(&meta_with(&[])).is_empty());
    }

    #[test]
    fn every_published_card_lands_on_the_roster_with_its_role() {
        let doc = meta_with(&[
            ("alice", card("endpoint-a", Role::Producer)),
            ("bob", card("endpoint-b", Role::Consumer)),
        ]);
        let roster = cards_from_meta(&doc);

        assert_eq!(roster.len(), 2);
        assert_eq!(
            roster["endpoint-a"].role.as_deref(),
            Some("producer"),
            "the role a peer published is what a source-selector reads"
        );
        assert_eq!(roster["endpoint-b"].role.as_deref(), Some("consumer"));
    }

    /// A peer is on the mesh before it has published anything.
    #[test]
    fn a_peer_with_no_card_yet_is_skipped() {
        let doc = serde_json::json!({
            "peers": {
                "alice": { "card": card("endpoint-a", Role::Producer) },
                "bob": {},
            }
        });
        let roster = cards_from_meta(&doc);
        assert_eq!(roster.len(), 1);
        assert!(roster.contains_key("endpoint-a"));
    }

    /// The one that matters: a peer we cannot parse must cost us that peer, not
    /// the roster. Otherwise one bad card from a future build blinds us to
    /// every good one.
    #[test]
    fn an_unparseable_card_costs_only_that_peer() {
        let doc = serde_json::json!({
            "peers": {
                "alice": { "card": card("endpoint-a", Role::Producer) },
                // No `endpoint` — the one field `from_card_value` requires.
                "mallory": { "card": { "version": "9.9.9" } },
                "eve": { "card": "not even an object" },
            }
        });
        let roster = cards_from_meta(&doc);
        assert_eq!(roster.len(), 1, "the good card must survive its neighbours");
        assert!(roster.contains_key("endpoint-a"));
    }

    /// Nicknames are random per join, so a rejoining peer appears under a new
    /// one. Keying by endpoint is what makes that one peer rather than two.
    #[test]
    fn one_peer_under_two_nicknames_collapses_to_one_entry() {
        let doc = meta_with(&[
            ("old-nick", card("endpoint-a", Role::Consumer)),
            ("new-nick", card("endpoint-a", Role::Consumer)),
        ]);
        assert_eq!(cards_from_meta(&doc).len(), 1);
    }

    fn peer(endpoint: &str, role: Role) -> PeerCard {
        PeerCard::new(
            endpoint,
            "0.1.0",
            "rust",
            "unicast",
            Some(role.as_card_str().to_owned()),
        )
    }

    /// The breakdown must agree with the count printed beside it, and that
    /// count already excludes self.
    #[test]
    fn the_roles_breakdown_never_counts_us() {
        let cards = vec![peer("me", Role::Consumer), peer("them", Role::Producer)];
        assert_eq!(roles_suffix(&cards, "me"), " (1 producing)");
        assert_eq!(
            roles_suffix(&[peer("me", Role::Consumer)], "me"),
            "",
            "a lone peer has nobody to describe"
        );
    }

    #[test]
    fn the_roles_breakdown_names_both_sides() {
        let cards = vec![
            peer("a", Role::Producer),
            peer("b", Role::Consumer),
            peer("c", Role::Consumer),
        ];
        assert_eq!(roles_suffix(&cards, "me"), " (1 producing, 2 reading)");
    }

    /// The roster legitimately lags the gossip counter — a peer is on the mesh
    /// before it publishes a card, and a future build may use a role word we do
    /// not know. Either way, say nothing rather than something wrong.
    #[test]
    fn an_unknown_or_missing_role_is_described_as_nothing() {
        let nameless = PeerCard::new("x", "0.1.0", "rust", "unicast", None);
        let future = PeerCard::new("y", "0.1.0", "rust", "unicast", Some("relay".to_owned()));
        assert_eq!(roles_suffix(&[nameless, future], "me"), "");
    }

    /// **Phase 0's actual claim**: two CLI peers of one share find each other on
    /// the mesh its ticket derives, and each can name the other's role.
    ///
    /// Everything above tests parsing against a synthetic document; this is the
    /// only test that exercises a real join, a real gossip round and the
    /// `on_meta_applied` hook that carries a card from one process's driver to
    /// another's roster.
    /// Bind an endpoint and join the share mesh `secret` derives, as `role`.
    async fn join_as(
        secret: &[u8; super::SECRET_LEN],
        lookups: &crate::protocol::swarm::LookupOpts,
        role: Role,
    ) -> super::ShareMesh {
        use agent_habilis_mesh::runtime::InjectedEndpoint;
        use fofoca_iroh_webrtc_transport::{WebRtcHandle, WebRtcTransport};
        use rand::RngCore;

        let mut key_bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut key_bytes);
        let key = iroh::SecretKey::from_bytes(&key_bytes);
        let webrtc = WebRtcHandle::new(WebRtcTransport::new(key.public()));
        let endpoint = crate::lookup::build_endpoint(
            lookups,
            Some(key),
            None,
            Vec::new(),
            Some(webrtc.clone()),
            false,
        )
        .await
        .expect("bind endpoint");
        super::join(super::JoinOpts {
            secret,
            lookups,
            shared: InjectedEndpoint { endpoint, webrtc },
            protocols: Vec::new(),
            role,
            transports: agent_habilis_mesh::net::TransportOpts::default(),
        })
        .await
        .expect("join the share mesh")
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn two_peers_of_one_share_see_each_other_on_the_mesh() {
        use crate::protocol::swarm::LookupOpts;
        use rand::RngCore;
        use std::time::{Duration, Instant};

        // One secret, so both peers derive the same mesh — the invariant the
        // whole design rests on.
        let mut secret = [0u8; super::SECRET_LEN];
        rand::rng().fill_bytes(&mut secret);
        let lookups = LookupOpts::loopback();

        let producer = join_as(&secret, &lookups, Role::Producer).await;
        let consumer = join_as(&secret, &lookups, Role::Consumer).await;
        assert_eq!(
            producer.mesh_id(),
            consumer.mesh_id(),
            "one ticket secret must derive one mesh"
        );

        // Poll rather than sleep a fixed time: gossip convergence is not
        // bounded, and a fixed sleep is either flaky or slow.
        let deadline = Instant::now() + Duration::from_secs(30);
        let seen = loop {
            let producer_sees_consumer = cards_from_book(&producer.book)
                .iter()
                .any(|card| card.role.as_deref() == Some("consumer"));
            let consumer_sees_producer = cards_from_book(&consumer.book)
                .iter()
                .any(|card| card.role.as_deref() == Some("producer"));
            if producer_sees_consumer && consumer_sees_producer {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        };

        assert!(
            seen,
            "each peer must find the other on the roster; producer saw {:?}, consumer saw {:?}",
            cards_from_book(&producer.book)
                .iter()
                .map(|card| card.role.clone())
                .collect::<Vec<_>>(),
            cards_from_book(&consumer.book)
                .iter()
                .map(|card| card.role.clone())
                .collect::<Vec<_>>(),
        );

        producer.leave().await;
        consumer.leave().await;
    }
}
