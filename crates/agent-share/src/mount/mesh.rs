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

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use agent_share_proto::PeerCard;
use agent_share_proto::framing::SECRET_LEN;
use agent_share_proto::mesh_key::share_mesh_key;
use agent_share_proto::roster::{Roster, entries_from_meta};
use anyhow::{Context, Result};
use fofoca::embed::{
    AppClass, EventLoopState, HandlerCtx, InboundApp, NodeApp, NodeDriver, SelfWriteGate,
    SilentSink,
};
use fofoca::net::TransportOpts;
use fofoca::ops::{StateMergeParams, broadcast_state_merge};
use fofoca::protocol::{Channel, Message, Nickname, Password};
use fofoca::runtime::{
    InjectedEndpoint, JoinParams, Node, Resolved, SetupParams, derive_topic_mesh_with,
};

/// A share's lookups, as the engine spells them.
///
/// The two types are structurally identical and carry the same `RelayUrl` —
/// the workspace pins one `iroh-base` — but they belong to different crates,
/// and deliberately: `agent-share-proto` is wasm-clean and depends on
/// `iroh-base` alone, so it must not learn about the engine just to spare this
/// function. The browser peer carries the same ten lines for the same reason.
pub(crate) fn mesh_lookups(
    share: &agent_share_proto::lookup::LookupOpts,
) -> fofoca::protocol::LookupOpts {
    use agent_share_proto::lookup::RelayChoice as ShareRelay;
    use fofoca::protocol::RelayChoice as MeshRelay;
    fofoca::protocol::LookupOpts {
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
use fofoca::net::MAX_DIRECT_PEERS;

/// Which side of the share a peer is on, as its meta card spells it.
///
/// The two strings are a vocabulary shared with the browser peer — `role` on
/// the meta card, rendered per peer by the web Info panel (`PeerRole` in
/// `packages/agent-share-web/src/lib/peerCard/index.ts`). An enum rather than a `&str` argument so a typo is a
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

/// Every peer's published card, plus the roster saying which of them are here.
///
/// Shared with [`ShareMesh`] rather than owned by the driver: the driver lives
/// inside the engine's event loop and is unreachable from the outside, so a
/// handle that outlives a borrow of it is the only way a caller can read the
/// roster. The browser peer carries the same split for the same reason.
/// `pub(crate)` because [`super::sources::SourceSet`] reads it too — the
/// roster is where read candidates come from.
pub(crate) type CardBook = Arc<Mutex<Roster>>;

/// The manifest fingerprint on our own card, shared with [`ShareMesh`].
///
/// A cell rather than a field because it is not known at join and does not stay
/// put: the browser joins inside its client constructor, before it has fetched
/// any manifest, and a producer's tree changes under `live.rs`'s rescan.
type SharedTree = Arc<Mutex<Option<String>>>;

/// What this peer advertises it can give.
///
/// The two travel together because they are published together and read
/// together: `serving` is the whole-slot contract a reader picks an `OP_READ`
/// source with, and `holding` says whether it is worth asking for chunks at all
/// — see [`agent_share_proto::PeerCard::holding`]. A mount part-way through a
/// transfer is `holding` with no `serving`, which is precisely the state the
/// pair exists to express.
#[derive(Clone, Default, PartialEq, Eq)]
pub(crate) struct Advertised {
    /// Slots servable whole, as [`agent_share_proto::serving`] encodes them.
    pub(crate) serving: Option<String>,
    /// Whether this peer holds any chunk of the share.
    pub(crate) holding: bool,
}

/// Shared like [`SharedTree`] and for the same reason: a mirror's coverage
/// changes as it fetches, so this cannot be fixed at join.
type SharedServing = Arc<Mutex<Advertised>>;

/// What the outside world can ask the driver to do.
///
/// The driver runs inside the engine's event loop, so this is the only way in.
/// One variant today; the availability grid will add its own rather than
/// widening this one into a general-purpose escape hatch.
pub(crate) enum ShareRequest {
    /// Re-publish our meta card, picking up whatever [`SharedTree`] now holds.
    RepublishCard,
}

/// The nicknames the engine currently counts as present, ours included.
///
/// `roster_snapshot()` reports active peers *and* the quiet ones it evicted
/// for silence; only the active half is present, which is exactly the set
/// behind the peer count (`tick_sweep` drops a quiet peer from `peers` and
/// rewrites that count in the same breath).
///
/// Our own nickname is added because we are never on our own roster and are
/// unquestionably here. Leaving it out would hide our own card.
fn present_nicknames(state: &EventLoopState, own: &Nickname) -> BTreeSet<String> {
    let mut names: BTreeSet<String> = state
        .roster_snapshot()
        .peers
        .into_iter()
        .filter(|entry| !entry.quiet)
        .map(|entry| entry.nickname.as_str().to_owned())
        .collect();
    names.insert(own.as_str().to_owned());
    names
}

/// Snapshot the present peers out of a shared book.
fn cards_from_book(book: &CardBook) -> Vec<PeerCard> {
    book.lock()
        .ok()
        .map(|book| book.present())
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
    /// The manifest fingerprint published on our card. See [`SharedTree`].
    tree: SharedTree,
    /// Which slots we advertise. See [`SharedServing`].
    serving: SharedServing,
    book: CardBook,
}

impl ShareDriver {
    fn new(
        version: String,
        runtime: String,
        transport: String,
        role: Option<String>,
        tree: SharedTree,
        serving: SharedServing,
        book: CardBook,
    ) -> Self {
        Self {
            version,
            runtime,
            transport,
            role,
            tree,
            serving,
            book,
        }
    }

    /// Rebuild the shared roster from the live meta document and the engine's
    /// own list of who is present.
    ///
    /// A full rebuild per event rather than a patch. The document is the
    /// authority and it is small — one card per peer — so re-reading it costs
    /// nothing next to the gossip round-trip that triggered it, and it cannot
    /// drift from what the CRDT actually says.
    ///
    /// Both halves are re-read together on purpose. They change on different
    /// events — a card arrives on the meta channel, a departure is an absence,
    /// an eviction is a timer — so reading one without the other is how the
    /// two drift into disagreeing about the same peer.
    fn refresh_book(&self, state: &EventLoopState, ctx: &HandlerCtx<'_>) {
        let next = Roster::new(
            entries_from_meta(&state.doc(Channel::Meta).to_json()),
            present_nicknames(state, ctx.author),
        );
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
        )
        .with_tree(self.tree.lock().ok().and_then(|tree| tree.clone()))
        .with_serving(
            self.serving
                .lock()
                .ok()
                .and_then(|advertised| advertised.serving.clone()),
        )
        .with_holding(self.serving.lock().ok().map(|advertised| advertised.holding));
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
        self.refresh_book(state, ctx);
    }
}

#[fofoca::async_trait]
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
        ctx: &HandlerCtx<'_>,
    ) {
        self.refresh_book(state, ctx);
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
        ctx: &HandlerCtx<'_>,
    ) {
        self.refresh_book(state, ctx);
    }
}

#[fofoca::async_trait]
impl NodeDriver for ShareDriver {
    type Session = ShareRequest;
    type Http = ();
    type Ipc = serde_json::Value;

    async fn on_startup(&mut self, state: &mut EventLoopState, ctx: &HandlerCtx<'_>) {
        self.publish_card(state, ctx).await;
    }

    /// The only signal a silence eviction gives an app.
    ///
    /// A peer evicted for going quiet produces no meta event and no
    /// `on_peer_left` — that hook fires for a graceful departure only, because
    /// a quiet peer may still return. Without this tick the shared roster
    /// would keep listing a peer the engine had already stopped counting,
    /// which is most of how the list and the count came to disagree.
    async fn on_tick(&mut self, state: &mut EventLoopState, ctx: &HandlerCtx<'_>) {
        self.refresh_book(state, ctx);
    }

    async fn handle_session(
        &mut self,
        req: ShareRequest,
        state: &mut EventLoopState,
        ctx: &HandlerCtx<'_>,
    ) -> bool {
        match req {
            ShareRequest::RepublishCard => {
                self.publish_card(state, ctx).await;
                // `true`: this broadcast, so the loop refreshes its
                // heartbeat-suppression clock rather than sending a redundant
                // beat straight after.
                true
            }
        }
    }
}

/// A live membership in a share's mesh.
pub(crate) struct ShareMesh {
    mesh_id: String,
    /// Held, not used: dropping every clone aborts the accept task, and this
    /// Router is now the only thing accepting the share's own ALPNs.
    _router: fofoca::iroh::protocol::Router,
    live: Arc<AtomicUsize>,
    webrtc: fofoca_iroh_webrtc_transport::WebRtcHandle,
    node: Option<Node<ShareDriver>>,
    book: CardBook,
    tree: SharedTree,
    serving: SharedServing,
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

    /// The live roster, shared: [`super::sources::SourceSet`] picks read
    /// candidates from it, and the dead-origin bootstrap in `consume` polls it
    /// for a peer that vouches.
    pub(crate) fn card_book(&self) -> CardBook {
        Arc::clone(&self.book)
    }

    /// Our own endpoint id — a candidate filter, never a candidate.
    pub(crate) fn local_endpoint(&self) -> &str {
        &self.local_endpoint
    }

    /// Publish the manifest fingerprint we are now on.
    ///
    /// Idempotent by value, and that *is* the debounce. A card rewrite is a CRDT
    /// merge broadcast to the whole mesh, and the callers here fire far more
    /// often than the value changes — `live.rs` rescans on a 300 ms timer and
    /// usually finds nothing different. Comparing before broadcasting turns that
    /// into silence, which a timer alone would not: a timer still sends the same
    /// value, just less often.
    ///
    /// Best-effort, like everything else on this mesh. A failed republish costs
    /// peers an up-to-date `tree`, never the share.
    pub(crate) async fn set_tree(&self, fingerprint: String) {
        {
            // Scoped so the lock is released before the await below; holding a
            // std `Mutex` across one is how an executor deadlocks itself.
            let Ok(mut current) = self.tree.lock() else {
                return;
            };
            if current.as_deref() == Some(fingerprint.as_str()) {
                return;
            }
            *current = Some(fingerprint);
        }
        let Some(node) = self.node.as_ref() else {
            return;
        };
        // Logged on the *success* path too, deliberately: this is the one place
        // that can flood the mesh with CRDT merges, so being able to see how
        // often it fires is what makes a runaway republish diagnosable rather
        // than just slow.
        match node.send(ShareRequest::RepublishCard).await {
            Ok(()) => tracing::debug!("republished the share card with a new tree"),
            Err(error) => tracing::debug!(%error, "republishing the share card failed"),
        }
    }

    /// Publish which manifest slots this peer can serve.
    ///
    /// Idempotent by value like [`Self::set_tree`], and for the same reason: a
    /// mirror recomputes this far more often than it changes, and a card
    /// rewrite is a CRDT merge sent to every peer.
    pub(crate) async fn set_serving(&self, encoded: Option<String>, holding: bool) {
        {
            let next = Advertised {
                serving: encoded,
                holding,
            };
            let Ok(mut current) = self.serving.lock() else {
                return;
            };
            if *current == next {
                return;
            }
            *current = next;
        }
        let Some(node) = self.node.as_ref() else {
            return;
        };
        match node.send(ShareRequest::RepublishCard).await {
            Ok(()) => tracing::debug!("republished the share card with new availability"),
            Err(error) => tracing::debug!(%error, "republishing the share card failed"),
        }
    }

    /// Report the peer counts on a line of their own whenever they change.
    ///
    /// Append-on-change rather than a redrawn status line: the CLI has no
    /// cursor-movement anywhere, and the bench consumer already sets this
    /// precedent. Quiet until the first peer arrives, so a solo share prints
    /// nothing extra.
    ///
    /// **Stdout is silent in `--output json`.** That stream is exactly one
    /// line — the mount command — and `tests/e2e_cli_webrtc.rs` and
    /// `tests/mount.rs` scrape it. Adding to it would break them, and rightly
    /// so. The tracing line below is not stdout and rides along either way.
    ///
    /// Besides the status line, this loop emits a **self-stats line every ten
    /// minutes** at INFO. The 2026-08-05 overnight incident (100% CPU by
    /// morning) was undiagnosable from its own log because nothing in it said
    /// what had accumulated; a counter snapshot per ten minutes — 144 lines a
    /// night — is what a post-mortem needs to say "the roster grew all night"
    /// or "it did not". Run long-lived serves with `RUST_LOG=info` and stderr
    /// captured, or the line has nowhere to land.
    pub(crate) fn spawn_report(&self, json: bool) {
        let live = Arc::clone(&self.live);
        let webrtc = self.webrtc.clone();
        let book = Arc::clone(&self.book);
        let local = self.local_endpoint.clone();
        tokio::spawn(async move {
            let mut last = None;
            let mut ticks: u32 = 0;
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                ticks = ticks.wrapping_add(1);
                if ticks.is_multiple_of(600) {
                    // `ghosts` is the whole reason this line exists: the meta
                    // document never forgets a peer, so the gap between what
                    // the roster shows and what the document holds is the
                    // overnight growth a post-mortem is looking for.
                    let (present, held) = book
                        .lock()
                        .ok()
                        .map(|book| (book.present().len(), book.all().len()))
                        .unwrap_or_default();
                    tracing::info!(
                        gossip = live.load(Ordering::Relaxed).saturating_sub(1),
                        direct = webrtc.transport().session_count(),
                        roster = present,
                        ghosts = held.saturating_sub(present),
                        "serve self-stats"
                    );
                }
                if json {
                    continue;
                }
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
pub(crate) struct JoinOpts {
    /// The mesh this peer joins, already resolved — and, on a protected share,
    /// already proven to match the password. See [`ShareMeshTarget`].
    ///
    /// Resolved by the caller rather than here because resolving is where a
    /// wrong password is *caught*, and that has to happen before an endpoint is
    /// bound and a dial is attempted, not inside a background join.
    pub(crate) target: ShareMeshTarget,
    /// The endpoint this process already speaks the mount protocol on, so the
    /// share and the mesh are one identity rather than two peers on one host.
    pub(crate) shared: InjectedEndpoint,
    /// ALPNs to serve on the mesh's Router. The producer's two ride here
    /// because iroh permits one accept loop per endpoint; a consumer serves
    /// none and passes an empty vec.
    pub(crate) protocols: Vec<(Vec<u8>, Box<dyn fofoca::iroh::protocol::DynProtocolHandler>)>,
    pub(crate) role: Role,
    /// Fingerprint of the manifest this peer is on, when it knows one.
    ///
    /// Both roles learn theirs *before* joining — the producer has scanned the
    /// tree, the consumer has fetched the manifest — so this is available at
    /// join rather than needing a later republish. A peer whose tree then
    /// changes under it goes stale; see the note on [`ShareDriver`].
    pub(crate) tree: Option<String>,
    /// Which slots this peer can serve at join, when it already knows. A
    /// producer knows immediately; a mirror recomputes as it fetches.
    pub(crate) serving: Option<String>,
    /// Must match the reach the injected endpoint was built with. A consumer
    /// run under `--transport webrtc` binds with IP cleared, so leaving the
    /// mesh on the default would have it advertise paths that do not exist.
    pub(crate) transports: TransportOpts,
}

/// What the share's mesh is, resolved once.
///
/// Holding the resolution rather than re-doing it is not only tidiness: on a
/// protected share resolving costs ~100 ms of Argon2id, and it is also the
/// moment a wrong password is caught. Both argue for doing it exactly once, at
/// a point the caller controls.
///
/// `Debug` prints the id and nothing else. The id is safe to print — it opens
/// nothing without the password — while the `Resolved` behind it holds the
/// stretched key.
pub(crate) struct ShareMeshTarget {
    /// What `setup_mesh` needs. Carries the `Mesh` with its stretched key
    /// already applied, so every derivation below it — the gossip topic, the
    /// rendezvous keypair, the port ladder — is behind the password, and the
    /// state/meta/broadcast documents are encrypted with keys off that same
    /// stretch.
    resolved: Resolved,
    /// The id a producer puts in its ticket. On a protected share this string
    /// carries the password verifier, which is the whole reason it has to
    /// travel: a joiner cannot derive those sixteen bytes for itself.
    mesh_id: String,
}

impl ShareMeshTarget {
    /// The mesh id, for a producer minting a ticket.
    pub(crate) fn mesh_id(&self) -> &str {
        &self.mesh_id
    }
}

impl std::fmt::Debug for ShareMeshTarget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ShareMeshTarget")
            .field("mesh_id", &self.mesh_id)
            .finish_non_exhaustive()
    }
}

/// The topic string a share's mesh is derived from.
///
/// Off the **secret**, not the token. The password is applied on top by
/// `fofoca`, which switches every derivation onto the stretched key
/// (`Mesh::effective_seed`) — so the mesh is still unreachable without the
/// password, but `fofoca` owns that gating rather than this crate re-deriving
/// around it. Hashed first because the engine carries the topic string into its
/// state file and user-facing lines.
fn topic_string(secret: &[u8; SECRET_LEN]) -> String {
    share_mesh_key(secret)
}

/// Build the mesh a **producer** serves under, minting its id.
///
/// # Errors
/// The derived id is unusable.
pub(crate) fn mint(
    secret: &[u8; SECRET_LEN],
    lookups: &agent_share_proto::lookup::LookupOpts,
    password: Option<&str>,
) -> Result<ShareMeshTarget> {
    // The share's own reach, not the public preset — a private share must not
    // stand up a public rendezvous it could never reach anyway.
    let mut mesh = derive_topic_mesh_with(&topic_string(secret), mesh_lookups(lookups))
        .context("deriving the share's mesh")?;
    if let Some(password) = password {
        // Bakes the verifier into the id and switches the derivations onto the
        // stretched key. The id is then what a joiner checks its password
        // against, with no network and no producer.
        mesh.set_password(&Password::new(password.to_owned()));
    }
    resolve_id(mesh.to_string(), password)
}

/// Resolve the mesh a **consumer** joins, checking the password locally.
///
/// `mesh_id` is what the ticket carried. `None` means the ticket predates the
/// field, so the id is derived the way a producer would — which still joins the
/// right mesh, but leaves nothing to check the password against, so a wrong one
/// is caught by the producer instead. That fallback is the *only* path where
/// the origin still has to be alive.
///
/// # Errors
/// The password is wrong, missing, or given for a share that has none.
pub(crate) fn resolve(
    mesh_id: Option<&str>,
    secret: &[u8; SECRET_LEN],
    lookups: &agent_share_proto::lookup::LookupOpts,
    password: Option<&str>,
) -> Result<ShareMeshTarget> {
    match mesh_id {
        Some(mesh_id) => resolve_id(mesh_id.to_owned(), password),
        None => mint(secret, lookups, password),
    }
}

/// Hand `mesh_id` to `fofoca` and let it rule on the password.
///
/// This is the check. `JoinParams::resolve` decodes the id, stretches the
/// password with Argon2id, and compares the result against the verifier the id
/// carries — locally, before any socket exists.
fn resolve_id(mesh_id: String, password: Option<&str>) -> Result<ShareMeshTarget> {
    let resolved = JoinParams {
        target: mesh_id.parse().context("parsing the share mesh id")?,
        nickname: Some(Nickname::random()),
        password: password.map(|password| Password::new(password.to_owned())),
    }
    .resolve()
    .map_err(explain_password_error)?;
    Ok(ShareMeshTarget { resolved, mesh_id })
}

/// Rewrite `fofoca`'s password errors into this tool's vocabulary.
///
/// Matched on the message because `fofoca` does not export a type for these
/// yet: `apply_password` fails with a bare `bail!("wrong password")` and its
/// `PasswordRequired` is `pub(crate)`, despite its own doc calling it "typed for
/// the frontends". Worth fixing there — until then a reword upstream silently
/// turns this back into a generic failure, which is why the fallback keeps the
/// original error rather than guessing.
fn explain_password_error(error: anyhow::Error) -> anyhow::Error {
    let rendered = format!("{error:#}").to_ascii_lowercase();
    if rendered.contains("wrong password") {
        return error.context(super::consume::WRONG_PASSWORD);
    }
    if rendered.contains("password") {
        return error;
    }
    error.context("resolving the share mesh join")
}

/// Join the mesh this share's ticket names, and return a handle to it.
///
/// # Errors
/// The node cannot bind an endpoint / reach a relay. Callers treat this as
/// non-fatal: the share serves either way.
pub(crate) async fn join(opts: JoinOpts) -> Result<ShareMesh> {
    let JoinOpts {
        target,
        shared,
        protocols,
        role,
        tree,
        serving,
        transports,
    } = opts;
    let Resolved { kind, author, .. } = target.resolved;

    // Read before `shared` is moved into the setup params below.
    let local_endpoint = shared.endpoint.id().to_string();
    let live = Arc::new(AtomicUsize::new(0));
    let config = fofoca::runtime::setup_mesh(
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
    let book: CardBook = Arc::new(Mutex::new(Roster::default()));
    let tree: SharedTree = Arc::new(Mutex::new(tree));
    let serving: SharedServing = Arc::new(Mutex::new(Advertised {
        holding: serving.is_some(),
        serving,
    }));
    let driver = ShareDriver::new(
        env!("CARGO_PKG_VERSION").to_owned(),
        "rust".to_owned(),
        "unicast".to_owned(),
        Some(role.as_card_str().to_owned()),
        Arc::clone(&tree),
        Arc::clone(&serving),
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
        tree,
        serving,
        local_endpoint,
    })
}

#[cfg(test)]
mod tests {
    use super::{Role, cards_from_book, roles_suffix};
    use agent_share_proto::PeerCard;
    use agent_share_proto::roster::{Roster, entries_from_meta};
    use std::collections::BTreeSet;

    /// How long a roster may take to converge before a test calls it stuck.
    ///
    /// A loopback mesh converges in well under a second; the headroom is for a
    /// loaded machine. Every loop below breaks the moment it converges, so a
    /// healthy run pays none of it.
    const CONVERGE: std::time::Duration = std::time::Duration::from_mins(1);

    /// Held by every test that binds real endpoints, so only one mesh is alive
    /// at a time.
    ///
    /// Four tests here stand up two or three engines each, and `cargo test`
    /// runs them alongside each other and ~125 other tests. Measured, that
    /// oversubscription made `peers_publish_the_tree_they_are_on` — the only
    /// three-engine row — miss its deadline about once in five workspace runs,
    /// with the code fine either way. Raising the deadline did not help: the
    /// failing run burned the whole 120 s, so the roster was not slow to
    /// converge, it never converged at all while starved.
    ///
    /// A tokio mutex rather than a `std` one because it is held across the
    /// awaits that drive the mesh. It also needs no poison handling: a test that
    /// panics is already a failure, and poisoning would turn one into four.
    static MESH_SLOT: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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

    /// The roster the driver would publish, given a document and who the
    /// engine says is here. The parse's own tolerances are pinned in
    /// `agent_share_proto::roster`; what is interesting here is the join.
    fn roster(doc: &serde_json::Value, present: &[&str]) -> Roster {
        Roster::new(
            entries_from_meta(doc),
            present.iter().map(|name| (*name).to_owned()).collect(),
        )
    }

    #[test]
    fn an_empty_document_yields_an_empty_roster() {
        assert!(
            roster(&serde_json::json!({}), &["alice"])
                .present()
                .is_empty()
        );
        assert!(roster(&meta_with(&[]), &["alice"]).present().is_empty());
    }

    #[test]
    fn every_published_card_lands_on_the_roster_with_its_role() {
        let doc = meta_with(&[
            ("alice", card("endpoint-a", Role::Producer)),
            ("bob", card("endpoint-b", Role::Consumer)),
        ]);
        let cards = roster(&doc, &["alice", "bob"]).present();

        assert_eq!(cards.len(), 2);
        let roles: BTreeSet<&str> = cards
            .iter()
            .filter_map(|card| card.role.as_deref())
            .collect();
        assert!(
            roles.contains("producer"),
            "the role a peer published is what a source-selector reads"
        );
        assert!(roles.contains("consumer"));
    }

    /// A peer is on the mesh before it has published anything, and a peer we
    /// cannot describe is not a row.
    #[test]
    fn a_peer_with_no_card_yet_is_skipped() {
        let doc = serde_json::json!({
            "peers": {
                "alice": { "card": card("endpoint-a", Role::Producer) },
                "bob": {},
            }
        });
        let cards = roster(&doc, &["alice", "bob"]).present();
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].endpoint, "endpoint-a");
    }

    /// **The defect this fixed, inverted.**
    ///
    /// A departed peer used to stay listed forever, because nothing deletes
    /// its CRDT entry and the list was built from the document alone. The
    /// availability grid then counted its slots as held, so "which slots would
    /// be lost" answered *none* when it should not. Three reloads of one tab
    /// reproduced it.
    ///
    /// An earlier filter against `roster_snapshot()` was tried and reverted
    /// because it made a *live* producer's card vanish: it compared an
    /// endpoint-keyed book against a nickname-keyed roster, a comparison that
    /// is empty however many peers are present. The join now happens on the
    /// nickname the document is itself keyed by, so the two are comparable —
    /// and the peer count is read off the same set, which is what makes the
    /// list and the count agree rather than merely resemble each other.
    #[test]
    fn a_peer_that_left_is_dropped_once_the_roster_drops_it() {
        let doc = meta_with(&[
            ("alice", card("endpoint-a", Role::Producer)),
            ("ghost", card("endpoint-gone", Role::Consumer)),
        ]);
        let book = roster(&doc, &["alice"]);
        let cards = book.present();
        let present: Vec<&str> = cards.iter().map(|card| card.endpoint.as_str()).collect();
        assert_eq!(
            present,
            vec!["endpoint-a"],
            "the departed peer is not a row"
        );
        assert_eq!(
            book.all().len(),
            2,
            "and its card is still in the document, which is why readers that \
             must not be starved use `all`"
        );
    }

    /// Nicknames are random per join, so a rejoining peer appears under a new
    /// one. Keying by endpoint is what makes that one peer rather than two.
    #[test]
    fn one_peer_under_two_nicknames_collapses_to_one_entry() {
        let doc = meta_with(&[
            ("old-nick", card("endpoint-a", Role::Consumer)),
            ("new-nick", card("endpoint-a", Role::Consumer)),
        ]);
        assert_eq!(roster(&doc, &["old-nick", "new-nick"]).present().len(), 1);
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
    /// Bind an endpoint and join the share mesh `token` derives, as `role`.
    async fn join_as(
        token: &[u8; super::SECRET_LEN],
        lookups: &crate::protocol::swarm::LookupOpts,
        role: Role,
    ) -> super::ShareMesh {
        join_as_on_tree(token, lookups, role, None).await
    }

    /// As [`join_as`], but publishing a manifest fingerprint.
    async fn join_as_on_tree(
        token: &[u8; super::SECRET_LEN],
        lookups: &crate::protocol::swarm::LookupOpts,
        role: Role,
        tree: Option<String>,
    ) -> super::ShareMesh {
        use fofoca::runtime::InjectedEndpoint;
        use fofoca_iroh_webrtc_transport::{WebRtcHandle, WebRtcTransport};
        use rand::RngCore;

        let mut key_bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut key_bytes);
        let key = fofoca::iroh::SecretKey::from_bytes(&key_bytes);
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
            target: super::mint(token, lookups, None).expect("mint the share mesh"),
            shared: InjectedEndpoint { endpoint, webrtc },
            protocols: Vec::new(),
            role,
            tree,
            serving: None,
            transports: fofoca::net::TransportOpts::default(),
        })
        .await
        .expect("join the share mesh")
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn two_peers_of_one_share_see_each_other_on_the_mesh() {
        use crate::protocol::swarm::LookupOpts;
        use rand::RngCore;
        use std::time::{Duration, Instant};

        let _slot = MESH_SLOT.lock().await;
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
        let deadline = Instant::now() + CONVERGE;
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

    /// **Phase 1's claim**: peers on one tree agree on its fingerprint, and a
    /// peer on a different tree is visibly different.
    ///
    /// This is what guard #1 will enforce. Here it only has to be *legible* —
    /// a reader can tell the two apart — but if the fingerprint did not survive
    /// the card round-trip, or two peers on one tree computed different
    /// strings, the guard would be built on sand.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn peers_publish_the_tree_they_are_on() {
        use crate::protocol::swarm::LookupOpts;
        use agent_share_proto::manifest::{FileEntry, MountManifest};
        use rand::RngCore;
        use std::time::{Duration, Instant};

        fn manifest_of(paths: &[&str]) -> MountManifest {
            MountManifest {
                dirs: Vec::new(),
                files: paths
                    .iter()
                    .map(|path| FileEntry {
                        rel_path: (*path).to_owned(),
                        size: 1,
                        mode: 0o644,
                        mtime: 0,
                    })
                    .collect(),
            }
        }

        let _slot = MESH_SLOT.lock().await;
        let shared = manifest_of(&["a.txt", "b.txt"]).fingerprint();
        let diverged = manifest_of(&["a.txt", "c.txt"]).fingerprint();
        assert_ne!(
            shared, diverged,
            "different trees must differ locally first"
        );

        let mut secret = [0u8; super::SECRET_LEN];
        rand::rng().fill_bytes(&mut secret);
        let lookups = LookupOpts::loopback();

        let origin = join_as_on_tree(&secret, &lookups, Role::Producer, Some(shared.clone())).await;
        let agreeing =
            join_as_on_tree(&secret, &lookups, Role::Consumer, Some(shared.clone())).await;
        let stale =
            join_as_on_tree(&secret, &lookups, Role::Consumer, Some(diverged.clone())).await;

        let deadline = Instant::now() + CONVERGE;
        let converged = loop {
            let seen = cards_from_book(&origin.book);
            let on_our_tree = seen
                .iter()
                .filter(|card| card.tree.as_ref() == Some(&shared));
            let elsewhere = seen
                .iter()
                .filter(|card| card.tree.as_ref() == Some(&diverged));
            // Two on ours counts the origin itself; one elsewhere is the stale peer.
            if on_our_tree.count() >= 2 && elsewhere.count() >= 1 {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        };

        assert!(
            converged,
            "the origin must see who shares its tree and who does not; roster was {:?}",
            cards_from_book(&origin.book)
                .iter()
                .map(|card| (card.role.clone(), card.tree.clone()))
                .collect::<Vec<_>>(),
        );

        origin.leave().await;
        agreeing.leave().await;
        stale.leave().await;
    }

    /// **The gap phase 1 shipped with.** A peer that does not know its tree at
    /// join must be able to publish one afterwards.
    ///
    /// This is not a corner case, it is the browser: `MeshPeer::join_share` runs
    /// inside the wasm client's constructor, before any manifest has been
    /// fetched. Without a republish path a browser peer advertises `tree: None`
    /// for its whole life, so guard #1 can never vouch for it and it is never a
    /// candidate source.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_peer_can_publish_its_tree_after_joining_without_one() {
        use crate::protocol::swarm::LookupOpts;
        use rand::RngCore;
        use std::time::{Duration, Instant};

        let _slot = MESH_SLOT.lock().await;
        let mut secret = [0u8; super::SECRET_LEN];
        rand::rng().fill_bytes(&mut secret);
        let lookups = LookupOpts::loopback();
        let fingerprint = "0f1e2d3c4b5a6978".to_owned();

        let watcher = join_as_on_tree(&secret, &lookups, Role::Producer, None).await;
        // Joins knowing nothing, exactly as the browser does.
        let late = join_as_on_tree(&secret, &lookups, Role::Consumer, None).await;

        // Wait until the watcher can see the late peer at all, so the assertion
        // below is about the *tree* rather than about roster convergence.
        let deadline = Instant::now() + CONVERGE;
        while cards_from_book(&watcher.book)
            .iter()
            .all(|card| card.role.as_deref() != Some("consumer"))
        {
            assert!(Instant::now() < deadline, "the late peer never appeared");
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        assert!(
            cards_from_book(&watcher.book)
                .iter()
                .all(|card| card.tree.is_none()),
            "nobody has published a tree yet"
        );

        late.set_tree(fingerprint.clone()).await;

        let publish_deadline = Instant::now() + CONVERGE;
        let seen = loop {
            if cards_from_book(&watcher.book)
                .iter()
                .any(|card| card.tree.as_ref() == Some(&fingerprint))
            {
                break true;
            }
            if Instant::now() >= publish_deadline {
                break false;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        };
        assert!(
            seen,
            "the late peer's tree must reach the roster; watcher saw {:?}",
            cards_from_book(&watcher.book)
                .iter()
                .map(|card| card.tree.clone())
                .collect::<Vec<_>>(),
        );

        watcher.leave().await;
        late.leave().await;
    }

    /// Republishing the value already published must not broadcast.
    ///
    /// A card rewrite is a CRDT merge sent to the whole mesh, and `live.rs`
    /// rescans on a 300 ms timer that usually finds nothing changed. Without
    /// this the mesh would carry a redundant merge several times a second per
    /// peer, forever.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn setting_the_same_tree_twice_is_silent() {
        use crate::protocol::swarm::LookupOpts;
        use rand::RngCore;

        let _slot = MESH_SLOT.lock().await;
        let mut secret = [0u8; super::SECRET_LEN];
        rand::rng().fill_bytes(&mut secret);
        let mesh = join_as_on_tree(
            &secret,
            &LookupOpts::loopback(),
            Role::Producer,
            Some("aaaabbbbccccdddd".to_owned()),
        )
        .await;

        // Same value: the cell must be left exactly as it was.
        mesh.set_tree("aaaabbbbccccdddd".to_owned()).await;
        assert_eq!(
            mesh.tree.lock().expect("tree lock").as_deref(),
            Some("aaaabbbbccccdddd")
        );

        // A different value does take.
        mesh.set_tree("1111222233334444".to_owned()).await;
        assert_eq!(
            mesh.tree.lock().expect("tree lock").as_deref(),
            Some("1111222233334444")
        );

        mesh.leave().await;
    }
}
