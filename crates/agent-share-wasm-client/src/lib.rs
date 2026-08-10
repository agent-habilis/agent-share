//! The browser client: read a share over WebRTC and/or the iroh relay.
//!
//! No web-specific protocol. This speaks the same `agent-share/mount/1` ALPN
//! the CLI does, over the same `fofoca-iroh-webrtc-transport`, using the same
//! `agent-share-proto` wire types — the browser is a peer, not a special case.
//!
//! # Transport modes
//!
//! [`ShareClient::connect`] takes an optional mode (`webrtc` | `relay` |
//! `dynamic`). Omit it for the default: **both on, WebRTC preferred** — try a
//! data channel first, then fall back to the iroh relay (ticket ladder, often
//! `relay.agent-habilis.com`) when ICE fails. Force `webrtc` or `relay` in
//! tests.
//!
//! # The two-connection dance (WebRTC path)
//!
//! iroh only fans a connect's Initial out to candidate paths **while the
//! remote has no selected path**, so a live connection cannot be upgraded onto
//! a newly attached transport. Hence:
//!
//! 1. Dial `agent-share/webrtc-signal/1` over the relay and swap one JSEP
//!    envelope each way.
//! 2. Open a **fresh** connection to `agent-share/mount/1` against an address
//!    carrying only the `WebRTC` custom addr.
//!
//! When host/mDNS and NAT hairpin both fail there is no ICE path left: TURN is
//! refused by policy, because this project already relays through its own iroh
//! relay and running a second relay at the ICE layer would mean operating two
//! systems for one job. Under `dynamic` a failed ICE simply uses that relay for
//! mount bytes; under `webrtc` it fails loudly.
//!
//! # There is no upgrade watcher
//!
//! A mount that settles on the relay keeps the relay for that connection's
//! life. Nothing promotes it to a data channel that becomes viable later —
//! and nothing ever did; the watcher named in old comments and todo entries
//! was never built. In-place upgrade is impossible (see above: iroh stops
//! fanning out once the remote selects a path), so an upgrade means a redial
//! plus a connection swap, and the only redial today is the natural
//! reconnect, which runs the webrtc-first dial again. Whoever builds
//! upgrade-by-redial: scope it by pairing — only browser↔browser bulk ever
//! stalled (see `probe_read`), a relay win against a native peer is safe to
//! retire — and reuse the waiting membership's hubs, or the redial drops
//! every channel the membership holds.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::rc::Rc;

use fofoca_chunks::{
    ChunkHash, ChunkMap, ChunkSource as _, ChunkStore as _, Coverage, FileId, IdbStore, Root,
    chunk_hash,
};
use std::sync::Arc;

use agent_share_proto::PeerCard;
use agent_share_proto::auth::ShareAuth;
use agent_share_proto::authorship::{PublicKey, SignedManifest};
use agent_share_proto::framing::{
    self, BENCH_ECHO_INTERVAL_SECS, DEFAULT_BENCH_DURATION_SECS, MAX_BENCH_ECHO_BYTES,
    MAX_BENCH_FILL_BYTES, MAX_MANIFEST_BYTES, MAX_READ_LEN, MOUNT_ALPN, SECRET_LEN,
    WEBRTC_SIGNAL_ALPN,
};
use agent_share_proto::lookup::{LookupOpts, RelayChoice};
use agent_share_proto::manifest::{ManifestDelta, MountManifest};
use agent_share_proto::mesh_key::share_mesh_key;
use agent_share_proto::ticket::{MountTicket, TICKET_KIND_BENCH_RELAY, TICKET_KIND_BENCH_WEBRTC};
use fofoca::iroh::endpoint::{Connection, presets};
use fofoca::iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey, TransportAddr};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

mod link;
mod live_state;
mod mesh;
mod produce;
mod seed;
mod swarm;
mod transport_mode;

pub use mesh::MeshPeer;
pub use transport_mode::TransportMode;

use fofoca_iroh_webrtc_transport::{
    BrowserHubTransport, BrowserSession, IceServers, MAX_ENVELOPE_BYTES, SignalEnvelope,
    WebRtcHandle, browser_offer, custom_addr, log_signal_sdps,
};

/// The endpoint the mount rides, offered to the mesh so both share one hub.
///
/// Only present on the WebRTC path. On the relay path there is no hub to share,
/// so the mesh builds its own endpoint as before — one identity is the goal, but
/// a relay-only viewer has no WebRTC sessions to miscount anyway.
struct MeshEndpoint {
    endpoint: Endpoint,
    webrtc: WebRtcHandle,
}

/// Cached ICE remote candidate for one peer endpoint id.
type IpCache = Rc<RefCell<HashMap<String, (Option<String>, Option<String>)>>>;

/// Per-peer transport meters, keyed by endpoint id string. See [`link`].
type BytesCache = Rc<RefCell<HashMap<String, link::Meter>>>;

/// Per-lane meters for the mount connection, keyed by [`link::path_label`].
///
/// Owned by [`ShareClient::sample_link`], read (never advanced) by
/// [`ShareClient::info`]. `TOTAL_LANE` holds the whole-connection figure.
type LinkCache = RefCell<HashMap<String, link::LaneMeter>>;

/// Cache key for the connection totals, which are not a path and so cannot
/// collide with a [`link::path_label`].
const TOTAL_LANE: &str = "total";

/// A connected share, ready to list and read.
#[wasm_bindgen]
pub struct ShareClient {
    connection: Connection,
    token: [u8; SECRET_LEN],
    /// The creator's authorship public key, from the ticket this tab opened.
    ///
    /// `None` for an unsigned share, which is accepted as it always was. It is
    /// held on the client rather than read per fetch because the answer must
    /// come from the link the user followed and not from whoever answered —
    /// see [`accept_manifest`].
    author: Option<[u8; 32]>,
    /// `"webrtc"` or `"relay"` — the path that actually carries mount bytes.
    ///
    /// A shared cell because on a dynamic WebRTC connect the label is
    /// provisional until path selection settles, and the settle happens in the
    /// background join task *after* `connect` has already handed the client to
    /// JS — the 3 s settle wait bought nothing but a label and does not belong
    /// on the connect path. `info_json` prefers the live selected path anyway.
    data_path: Rc<RefCell<String>>,
    /// Requested connect mode (`webrtc` / `relay` / `dynamic`).
    mount_mode: String,
    /// Why `dynamic` ended up on the relay, when it did. `None` on a clean
    /// connect. Surfaced on the info pane: a fallback that only warns to the
    /// console is a fallback nobody can diagnose from the UI. Shared for the
    /// same reason as `data_path`.
    fallback_reason: Rc<RefCell<Option<String>>>,
    /// Relay URLs of the *signal* endpoint, captured before it moves into the
    /// mesh. The mount endpoint is relay-free on the WebRTC path, so this is
    /// the only place the rendezvous relay is still observable.
    rendezvous_relays: Vec<String>,
    /// Producer ticket lookups — labeled "producer reach" in the info pane.
    lookups: LookupOpts,
    /// `js_sys::Date::now()` when connect resolved (UI wall clock).
    connected_at_ms: f64,
    /// Last-known getStats IPs, keyed by endpoint id string.
    ip_cache: IpCache,
    /// Last-known candidate-pair counters, keyed by endpoint id string.
    ///
    /// The `getStats` half of [`link`] — per peer, and `WebRTC`-only.
    bytes_cache: BytesCache,
    /// Last-known QUIC counters for the mount connection, per lane.
    ///
    /// The other half of [`link`], and the one that answers on the relay path
    /// too. Advanced only by [`Self::sample_link`].
    link_cache: LinkCache,
    // Held so the hub (and its data channel) outlives the connection when used.
    _hub: Option<Arc<BrowserHubTransport>>,
    _session: Option<BrowserSession>,
    _endpoint: Endpoint,
    /// Handed to the mesh so the mount session and every mesh session land in
    /// one hub, under one identity. Taken at `connect`.
    mesh_endpoint: Option<MeshEndpoint>,
    /// This tab's membership in the share's mesh — the thing that makes two
    /// viewers of one share peers rather than strangers. `None` when the mesh
    /// could not be joined; the share itself still works, so this is never
    /// allowed to fail a connect.
    ///
    /// Behind a `RefCell` so that [`Self::leave_mesh`] can take `&self`, which
    /// is load-bearing rather than stylistic. wasm-bindgen holds an object
    /// borrowed for the **entire lifetime of the future** returned by an async
    /// `&self` method, and this type has four of them ([`Self::read`],
    /// [`Self::manifest`], [`Self::watch`], [`Self::refresh_peer_ips`]). A
    /// `&mut self` method called while any one of those is still pending
    /// therefore panics with "recursive use of an object detected which would
    /// lead to unsafe aliasing in Rust" — which is exactly what a revival did,
    /// since it drops the old client via `leave_mesh` while reads may still be
    /// parked on the connection that just died.
    ///
    /// So `ShareClient` deliberately exposes **no** `&mut self` method. That is
    /// the invariant; this field is how it is kept, and why `store` and `held`
    /// below are behind one too.
    ///
    /// `Rc` inside the cell, because a `RefCell` solves the `&mut self` problem
    /// only to hand back a borrow one. [`Self::publish_serving`] and
    /// [`Self::manifest`] both `await` on the peer, and holding
    /// `self.mesh.borrow()` across a yield point is what earns the *other*
    /// panic: `leave_mesh` takes `borrow_mut` and fires from `pagehide` or a
    /// revival at any moment. Cloning the `Rc` out first ends the borrow before
    /// the await, the same move [`Self::refresh_peer_ips`] makes with the hub.
    ///
    /// Tri-state rather than `Option` because the join runs in a background
    /// task after `connect` resolves: `Pending` means the join is (possibly)
    /// still in flight, and `Left` records a client released *before* it
    /// landed — the join task observes `Left` and leaves the fresh membership
    /// instead of installing it, so a mid-join release cannot leak one.
    mesh: Rc<RefCell<MeshSlot>>,
    /// The last manifest fingerprint this client learned, kept so the
    /// background join can publish `set_tree` even when the first manifest
    /// fetch beat it — otherwise a browse-only tab's card would never carry a
    /// tree and seeder vouching would regress.
    last_tree: Rc<RefCell<Option<String>>>,
    /// Whether the background join task still owes a `settled_path_label`
    /// pass to firm up `data_path` / `fallback_reason`. Set only on the
    /// dynamic WebRTC mount path; every other path knows its label at build.
    settle_pending: bool,
    /// A manifest already in hand when the client was built or shortly after,
    /// consumed by the first [`Self::fetch_manifest`]. Two producers: the
    /// seeder fallback, which fetches and vets a manifest to pick a winner
    /// and used to throw it away (the App then re-paid a relay-hop RTT for
    /// the same bytes), and an origin-path background prefetch issued the
    /// moment the connection exists. Single-consume; a fetch that finds it
    /// empty just fetches live.
    prefetched_manifest: PrefetchedManifest,
    /// Set once a live manifest fetch has answered; a prefetch landing after
    /// that stays out of the cell so a later sync cannot consume bytes older
    /// than what the client already saw.
    manifest_fetched: Rc<Cell<bool>>,
    /// Bytes this tab holds, and can therefore seed.
    ///
    /// Opened on the first sync rather than at connect: a tab that only browses
    /// should not create a database, and `IndexedDB` can be refused outright in
    /// private mode — which must cost seeding, never the share.
    store: RefCell<Option<Arc<IdbStore>>>,
    /// Manifest indices fully held, so the UI can mark what is seedable and the
    /// card can advertise it.
    ///
    /// Shared so [`watch_for_lost_holdings`] can shrink it: an eviction has to
    /// correct the grid and the card together, or the UI keeps promising bytes
    /// the mesh has already been told are gone.
    card: Rc<RefCell<CardHoldings>>,
    /// Chunk rows for slots this tab has learned, by manifest index.
    ///
    /// The row is what makes a chunk addressable: without it an address is
    /// just 32 bytes, and this tab could neither ask for the right ones nor
    /// scope what it answers for. Learned lazily — one `OP_CHUNK_MAP` per file
    /// somebody actually wants — and kept in the store so a reload re-arms.
    rows: RefCell<HashMap<u32, ChunkMap>>,
    /// The serving half of seeding: the mount-protocol source registered on
    /// the mesh Router at connect, fed by [`Self::sync`] /
    /// [`Self::refresh_held`]. Empty until the first sync, and an empty
    /// seeder refuses requests rather than answering for a tree it cannot
    /// back.
    seeder: seed::SeederShared,
    /// Whether the mount connection reaches the ticket's **origin**, or a
    /// seeder that vouched for its tree. Authority hangs off this: manifests
    /// from a seeder are a frozen snapshot, and a short read from one is a
    /// failure rather than EOF (guards #1/#2).
    from_origin: bool,
    /// The tree a seeder connection was vetted against at connect — the
    /// majority tree among vouching cards. Every later manifest fetch is
    /// checked against it, so a seeder cannot swap trees after winning the
    /// dial. `None` on origin connections: the origin is the one peer with
    /// the *right* to change the tree.
    pinned_tree: Option<String>,
    /// The mount identity's relay-bearing half, kept for dialling other peers.
    ///
    /// A **second handle** rather than a read of `mesh_endpoint`, which is
    /// `take`n at connect and handed to the background mesh join. A dial that
    /// reached for it there would always find `None` — which is exactly the bug
    /// that made the swarm below silently never form, on the one path where it
    /// helps most.
    signal_endpoint: Option<Endpoint>,
    /// Extra peers to pull chunks from, beyond the one this tab is homed on.
    ///
    /// Deliberately *beside* `connection` rather than replacing it. The home
    /// connection is what the info pane samples, what `watch` follows, what a
    /// close is detected on, and what a password refusal surfaces through —
    /// one peer with a distinguished role. These are extra bandwidth for bulk
    /// only, so a swarm that fails to form costs nothing but the speed.
    ///
    /// Dialled once, lazily, on the first transfer that could use them, and
    /// kept afterwards: a dial costs a JSEP round, which is far too expensive
    /// to pay per file.
    swarm: RefCell<Vec<SwarmPeer>>,
    /// Whether the lazy dial above has already run.
    swarm_dialled: Cell<bool>,
    /// Whether [`watch_for_lost_holdings`] is already running for this client.
    retraction_watched: Cell<bool>,
}

/// One extra source of chunks.
struct SwarmPeer {
    connection: Connection,
    /// The peer's endpoint id, for the log line that says where bytes came
    /// from. A transfer spread over peers is otherwise impossible to explain
    /// after the fact.
    endpoint: String,
}

/// How many extra peers to dial for bulk.
///
/// Small on purpose. Each one costs a JSEP round and a data channel, and the
/// gain flattens quickly — the point of a swarm here is that the *rare* chunk
/// gets a second holder, not that a file arrives on twelve connections. The
/// home connection is not counted, so this is a ceiling of four sources.
const SWARM_WIDTH: usize = 3;

/// `author` is the ticket's authorship key, and is a parameter rather than a
/// field set afterwards on purpose: a connect path that forgot it would verify
/// nothing and look completely healthy while doing so.
fn new_share_client(
    connection: Connection,
    token: [u8; SECRET_LEN],
    author: Option<[u8; 32]>,
    data_path: String,
    hub: Option<Arc<BrowserHubTransport>>,
    session: Option<BrowserSession>,
    mesh_endpoint: Option<MeshEndpoint>,
    endpoint: Endpoint,
) -> ShareClient {
    // A webrtc-carried mount gets a health watcher on its underlying
    // RtcPeerConnection. Without one, a dead channel (Wi-Fi→cellular
    // handoff, laptop wake) is only noticed when QUIC gives up on the idle
    // connection — tens of seconds of a tab that looks connected and fails
    // every read. The watcher closes the mount as soon as the channel is
    // beyond saving, and the app's liveness poll takes it from there.
    if data_path == "webrtc"
        && let Some(hub) = hub.as_ref()
    {
        watch_channel_health(Arc::clone(hub), connection.clone());
    }
    ShareClient {
        connection,
        token,
        author,
        data_path: Rc::new(RefCell::new(data_path)),
        mount_mode: "dynamic".to_owned(),
        fallback_reason: Rc::new(RefCell::new(None)),
        rendezvous_relays: Vec::new(),
        lookups: LookupOpts::public_preset(),
        connected_at_ms: now_ms(),
        ip_cache: Rc::new(RefCell::new(HashMap::new())),
        bytes_cache: Rc::new(RefCell::new(HashMap::new())),
        link_cache: RefCell::new(HashMap::new()),
        _hub: hub,
        _session: session,
        signal_endpoint: mesh_endpoint.as_ref().map(|shared| shared.endpoint.clone()),
        mesh_endpoint,
        _endpoint: endpoint,
        mesh: Rc::new(RefCell::new(MeshSlot::Pending)),
        last_tree: Rc::new(RefCell::new(None)),
        settle_pending: false,
        prefetched_manifest: Rc::new(RefCell::new(None)),
        manifest_fetched: Rc::new(Cell::new(false)),
        swarm: RefCell::new(Vec::new()),
        swarm_dialled: Cell::new(false),
        retraction_watched: Cell::new(false),
        store: RefCell::new(None),
        card: Rc::new(RefCell::new(CardHoldings::default())),
        rows: RefCell::new(HashMap::new()),
        seeder: seed::SeederShared::new(),
        from_origin: true,
        pinned_tree: None,
    }
}

/// How long the channel-health watcher lets `disconnected` stand before
/// declaring the channel dead.
///
/// `disconnected` is the one recoverable state — ICE flaps through it on a
/// brief radio blip and often comes back on its own — so it gets a grace
/// window. `failed` and `closed` are terminal (this stack has no
/// renegotiation path: JSEP here is one envelope each way, so nothing can
/// drive a `restartIce` to completion) and are acted on immediately.
const CHANNEL_DISCONNECT_GRACE_MS: f64 = 10_000.0;

/// What the channel-health watcher should do about one observation.
#[derive(Debug, PartialEq, Eq)]
enum ChannelVerdict {
    /// The channel is fine (or recovered); clear any pending grace window.
    Healthy,
    /// Disconnected, but inside the grace window — keep watching.
    Wait,
    /// Beyond saving: close the mount so recovery starts now.
    Kill(&'static str),
}

/// Pure judgment for one poll tick: the peer connection's state plus how
/// long a disconnect has been standing. Split from the loop so the grace
/// hysteresis is testable without an `RtcPeerConnection`.
fn judge_channel(
    state: web_sys::RtcPeerConnectionState,
    disconnected_for_ms: Option<f64>,
) -> ChannelVerdict {
    use web_sys::RtcPeerConnectionState as State;
    match state {
        State::Failed => ChannelVerdict::Kill("the data channel's ICE failed"),
        State::Closed => ChannelVerdict::Kill("the peer connection closed under the mount"),
        State::Disconnected => match disconnected_for_ms {
            Some(elapsed) if elapsed >= CHANNEL_DISCONNECT_GRACE_MS => {
                ChannelVerdict::Kill("the data channel stayed disconnected past the grace window")
            }
            _ => ChannelVerdict::Wait,
        },
        _ => ChannelVerdict::Healthy,
    }
}

/// Watch a webrtc-carried mount's peer connection and close the mount the
/// moment the channel is beyond saving.
///
/// Nothing else observes ICE at all — the transport reads it once during
/// setup and never again — so without this the only signal is QUIC's idle
/// timeout, noticed by the app's 1 s `closed` poll long after the wire went
/// dark. A poll rather than a `connectionstatechange` listener on purpose:
/// a listener would race the transport for the handler slot (or leak a
/// forgotten closure per reconnect), while reading a property once a second
/// costs nothing and ends itself with the connection.
fn watch_channel_health(hub: Arc<BrowserHubTransport>, connection: Connection) {
    let remote = connection.remote_id();
    wasm_bindgen_futures::spawn_local(async move {
        let mut disconnected_since: Option<f64> = None;
        loop {
            wait_ms(1_000).await;
            if connection.close_reason().is_some() {
                return;
            }
            let Some(peer_connection) = hub.peer_connection(&remote) else {
                // The transport dropped the session: the channel is gone and
                // nothing will rebuild it in place.
                connection.close(0u32.into(), b"data channel session detached");
                return;
            };
            let state = peer_connection.connection_state();
            let elapsed = disconnected_since.map(|since| now_ms() - since);
            match judge_channel(state, elapsed) {
                ChannelVerdict::Healthy => disconnected_since = None,
                ChannelVerdict::Wait => {
                    if disconnected_since.is_none() {
                        disconnected_since = Some(now_ms());
                        web_sys::console::log_1(&JsValue::from_str(
                            "[share] data channel disconnected; giving ICE its grace window",
                        ));
                    }
                }
                ChannelVerdict::Kill(reason) => {
                    web_sys::console::log_1(&JsValue::from_str(&format!(
                        "[share] {reason}; closing the mount so recovery starts now"
                    )));
                    connection.close(0u32.into(), b"data channel died");
                    return;
                }
            }
        }
    });
}

/// A manifest fetched ahead of the first request — see the field on
/// [`ShareClient`].
type PrefetchedManifest = Rc<RefCell<Option<FetchedManifest>>>;

/// What one `OP_MANIFEST` round produced, already checked against the ticket.
///
/// Two byte strings rather than one, because they answer different questions
/// and using either for the other's job is a silent bug. Naming them apart is
/// the cheapest way to stop that.
#[derive(Clone)]
struct FetchedManifest {
    /// The wire body: `version ‖ signature ‖ manifest`. What a seeder re-serves
    /// and what a tab persists — a copy that kept only the manifest could never
    /// prove anything to the next reader, since a browser holds no authorship
    /// key and cannot re-sign.
    envelope: Vec<u8>,
    /// The manifest alone, which is the fingerprint domain: `card.tree` and
    /// every `pinned_tree` comparison are defined over exactly these bytes.
    body: Vec<u8>,
    manifest: MountManifest,
}

/// This tab's relationship to the share's mesh — see the `mesh` field.
enum MeshSlot {
    /// No membership yet; the background join may still install one.
    Pending,
    /// Joined; the peer is live.
    Joined(Rc<mesh::MeshPeer>),
    /// The client let go (release, ticket change, shutdown). A join landing
    /// now must leave rather than install.
    Left,
}

#[wasm_bindgen]
impl ShareClient {
    /// Whether redeeming `ticket` needs a password, read from the ticket alone.
    ///
    /// Cheap and offline — it decodes a flag, it does not derive anything — so
    /// a page can put its password form up before paying the ~100 ms Argon2id
    /// that [`Self::connect`] costs on a protected share, and before opening a
    /// socket at all.
    ///
    /// # Errors
    /// The ticket is malformed.
    #[wasm_bindgen]
    pub fn password_required(ticket: String) -> Result<bool, JsValue> {
        let ticket = MountTicket::decode(&ticket).map_err(|error| err("decode ticket", &error))?;
        Ok(ticket.password_protected())
    }

    /// Decode `ticket` and open the mount connection.
    ///
    /// `transport` is `webrtc`, `relay`, or `dynamic` (case-insensitive).
    /// Omit it for **dynamic**: both paths on, WebRTC preferred, iroh relay
    /// fallback. See [`TransportMode`].
    ///
    /// `card` is the peer identity the **TypeScript consumer** wants published
    /// on meta (`{ version, runtime, transport?, role? }`). Wasm does not
    /// sniff the browser — omit `transport` to use the mount data path.
    ///
    /// # Errors
    /// The ticket is malformed, the mode is unknown, the producer is
    /// unreachable, or (in `webrtc` mode) ICE fails with no fallback.
    #[wasm_bindgen]
    /// `origin_cap_ms` bounds the origin dial before the seeder fallback
    /// takes over; omit it for [`ORIGIN_DIAL_CAP_MS`]. The revival path
    /// passes a tight one — it already knows the origin just died, and its
    /// whole attempt has to fit the App's reconnect budget.
    /// The tree this tab last seeded for `ticket`, straight from its own
    /// storage — no dial, no mesh, no peer. Lets the page render the share
    /// while a reconnect grinds underneath; the manifest is the same
    /// fingerprint-checked sidecar the seeder re-arm trusts. `undefined`
    /// when this tab never seeded here, storage was cleared, or the record
    /// fails its integrity check.
    ///
    /// On a protected share the password gates the peek the same way it
    /// gates a dial: a wrong one derives a different token, whose storage
    /// keys hold nothing.
    ///
    /// # Errors
    /// The ticket is malformed, or the share wants a password that was not
    /// supplied.
    #[wasm_bindgen]
    pub async fn peek_persisted_manifest(
        ticket: String,
        password: Option<String>,
    ) -> Result<JsValue, JsValue> {
        console_error_panic_hook::set_once();
        let ticket = MountTicket::decode(&ticket).map_err(|error| err("decode ticket", &error))?;
        let auth = redeem_auth(&ticket, password.as_deref())?;
        match load_persisted_manifest(auth.token()).await {
            Some((_, _, manifest, _)) => serde_wasm(&manifest),
            None => Ok(JsValue::UNDEFINED),
        }
    }

    pub async fn connect(
        ticket: String,
        transport: Option<String>,
        card: Option<JsValue>,
        origin_cap_ms: Option<f64>,
        password: Option<String>,
    ) -> Result<ShareClient, JsValue> {
        console_error_panic_hook::set_once();
        let mode = TransportMode::parse(transport.as_deref())
            .map_err(|message| JsValue::from_str(&message))?;
        let ticket = MountTicket::decode(&ticket).map_err(|error| err("decode ticket", &error))?;
        // The credential, derived once. On an unprotected share this is the
        // ticket secret verbatim; on a protected one it costs ~100 ms of
        // Argon2id on the main thread, which is why it happens here and not
        // per request. `password_required` lets the page know to collect a
        // password before paying it.
        let auth = redeem_auth(&ticket, password.as_deref())?;
        // Kept for the mesh join below: fofoca checks the password against the
        // verifier the ticket's mesh id carries, which is what names a wrong
        // password without a producer.
        let mesh_id = ticket.mesh_id.clone();
        // Copied out before the ticket moves into the connect race below; the
        // background prefetch has to verify against the same key the client
        // will.
        let author = ticket.author;
        let mesh_password = password.clone();
        // Resolved *before* the dial. On a protected share whose ticket carries
        // a mesh id, this is the check: fofoca decodes the id, stretches the
        // password, and compares it against the verifier. It fails here — with
        // no socket and no producer — which is the whole point, because a share
        // outlives its producer and a check that needs one usually cannot run.
        let resolved_mesh = mesh::resolve_share(mesh::ShareMeshRef {
            mesh_id: mesh_id.as_deref(),
            secret: &ticket.secret,
            lookups: &ticket.lookups,
            password: mesh_password.as_deref(),
        })?;
        // The raw bytes, for the request headers, the mesh id and the store
        // name. `auth` keeps the flag beside them, for the seeding half.
        let token = *auth.token();
        // Captured before `ticket` is moved into the connect. The mesh is
        // derived from the share's own reach, so every holder of this ticket —
        // producer included — computes the same mesh id.
        let lookups = ticket.lookups.clone();
        // Kept whole for the seeder fallback below — the dial consumes its copy.
        let fallback_ticket = ticket.clone();
        #[expect(
            clippy::cast_possible_truncation,
            reason = "a dial cap in ms is far inside i32"
        )]
        let cap_ms = origin_cap_ms.map_or_else(default_origin_cap_ms, |ms| (ms as i32).max(1_000));
        let dialed = match mode {
            TransportMode::Relay => {
                capped_origin_dial(Box::pin(connect_relay(ticket, token)), cap_ms, None).await
            }
            TransportMode::WebRtc => {
                connect_webrtc(ticket, token, /*allow_relay_fallback=*/ false).await
            }
            // Race the lanes rather than sequencing them. Sequenced, a dead
            // origin cost the whole dial cap before card collection even
            // began; raced, a dead-origin connect is bounded by card
            // collection alone. The head start keeps healthy shares out of
            // the race — a live origin answers well inside it — and the
            // waiting membership the seeder lane builds persists in
            // [`WAITING_MESHES`] either way, so a lost race costs no
            // identity churn.
            TransportMode::Dynamic => {
                // The lane coupling: the seeder lane raises this the moment a
                // peer answers its dial, and the origin dial concedes rather
                // than running out its cap against an origin the mesh has
                // already replaced.
                let can_serve = MeshCanServe::default();
                let origin = Box::pin(capped_origin_dial(
                    Box::pin(connect_webrtc(
                        ticket, token, /*allow_relay_fallback=*/ true,
                    )),
                    cap_ms,
                    Some((can_serve.clone(), ORIGIN_CONCEDE_FLOOR_MS)),
                ));
                let race_ticket = fallback_ticket.clone();
                let race_card = card.clone();
                let race_password = mesh_password.clone();
                let seeder = Box::pin(async move {
                    wait_ms(SEEDER_RACE_HEAD_START_MS).await;
                    let origin_status = JsValue::from_str(
                        "the origin had not answered when the seeder lane started",
                    );
                    connect_via_seeder(
                        race_ticket,
                        auth,
                        race_password.as_deref(),
                        race_card,
                        &origin_status,
                        Some(can_serve),
                    )
                    .await
                });
                match futures::future::select(origin, seeder).await {
                    // Origin won: drop the seeder attempt mid-flight. Its
                    // waiting membership persists by design, and the join
                    // task below says goodbye to the duplicate.
                    futures::future::Either::Left((Ok(client), _seeder)) => Ok(client),
                    // Origin is dead; the seeder lane is already warm.
                    futures::future::Either::Left((Err(origin_error), seeder)) => {
                        match seeder.await {
                            Ok(mut client) => {
                                client.mount_mode = mode.as_str().to_owned();
                                return Ok(client);
                            }
                            Err(seeder_error) => Err(JsValue::from_str(&format!(
                                "{} (origin: {})",
                                describe(&seeder_error),
                                describe(&origin_error),
                            ))),
                        }
                    }
                    // Seeder won: the dropped origin dial aborts
                    // un-`close()`d, exactly as the cap timeout always did.
                    futures::future::Either::Right((Ok(mut client), _origin)) => {
                        client.mount_mode = mode.as_str().to_owned();
                        return Ok(client);
                    }
                    // The seeder lane failed early (no relay in the ticket,
                    // no vouching cards); the origin may still answer. No
                    // second sequential seeder pass: the App retries connect,
                    // and the next attempt's lane reuses the warm membership.
                    futures::future::Either::Right((Err(seeder_error), origin)) => {
                        origin.await.map_err(|origin_error| {
                            JsValue::from_str(&format!(
                                "{} (seeder lane: {})",
                                describe(&origin_error),
                                describe(&seeder_error),
                            ))
                        })
                    }
                }
            }
        };
        let mut client = match dialed {
            Ok(client) => client,
            // The origin is unreachable — dead, or gone from the relay. The
            // share does not have to be: every holder of this link is on the
            // mesh it derives, and a peer whose card vouches for the tree can
            // serve it. Reached from `relay` mode only — `webrtc` mode exists
            // to pin the transport for tests (and the seeder lane rides the
            // relay), and `dynamic` already raced the seeder lane above.
            Err(origin_error) if matches!(mode, TransportMode::Relay) => {
                let mut client = connect_via_seeder(
                    fallback_ticket,
                    auth,
                    mesh_password.as_deref(),
                    card.clone(),
                    &origin_error,
                    None,
                )
                .await?;
                client.mount_mode = mode.as_str().to_owned();
                return Ok(client);
            }
            Err(origin_error) => return Err(origin_error),
        };
        client.mount_mode = mode.as_str().to_owned();
        client.lookups = lookups.clone();
        client.connected_at_ms = now_ms();
        // Join the share's mesh so this tab can see — and hold direct sessions
        // with — the other people viewing the same share. Strictly additive:
        // a mesh that will not start costs the peer counts and nothing else,
        // so it must never turn a working share into a failed connect — and,
        // since it is additive, it runs in the background instead of holding
        // the connect (its relay work cost real time on the critical path).
        let shared = client
            .mesh_endpoint
            .take()
            .map(|shared| (shared.endpoint, shared.webrtc));
        // Parsed eagerly so a malformed card from JS still fails the connect,
        // with the provisional transport label; the settle below patches it.
        let mut card = match card.as_ref() {
            Some(value) => mesh::parse_card_parts(
                value,
                &client.data_path.borrow(),
                Some("consumer".to_owned()),
            )?,
            None => {
                mesh::default_card_parts(&client.data_path.borrow(), Some("consumer".to_owned()))
            }
        };
        // The serving half of seeding: this viewer answers the share's own
        // ALPNs on the mesh Router, exactly the producer's shape. The mount
        // handler is backed by the tab's store and refuses everything until
        // the first sync fills it; the signal handler (WebRTC path only —
        // the relay path has no hub) lets another peer negotiate a data
        // channel to *us* the way we negotiate one to the producer.
        let mut protocols: Vec<(Vec<u8>, Box<dyn fofoca::iroh::protocol::DynProtocolHandler>)> =
            vec![(
                MOUNT_ALPN.to_vec(),
                Box::new(produce::MountHandler::new(client.seeder.clone(), auth)),
            )];
        if let Some((endpoint, webrtc)) = shared.as_ref() {
            protocols.push((
                WEBRTC_SIGNAL_ALPN.to_vec(),
                Box::new(produce::SignalHandler::new(
                    endpoint.id(),
                    webrtc.transport(),
                )),
            ));
        }
        let connection = client.connection.clone();
        let data_path = Rc::clone(&client.data_path);
        let fallback_reason = Rc::clone(&client.fallback_reason);
        let mesh_slot = Rc::clone(&client.mesh);
        let last_tree = Rc::clone(&client.last_tree);
        let settle_pending = client.settle_pending;
        wasm_bindgen_futures::spawn_local(async move {
            // Firm up the path label first: the card should advertise what the
            // mount actually selected, and the settle wait no longer holds the
            // connect. On a relay-free endpoint there is nothing to settle on
            // *but* WebRTC, so a different answer is a "selection never
            // settled" report, not a lost race.
            if settle_pending {
                let selected = settled_path_label(&connection).await;
                if let Some(label) = selected.clone() {
                    *data_path.borrow_mut() = label;
                }
                if selected.as_deref() != Some("webrtc") {
                    *fallback_reason.borrow_mut() = Some(format!(
                        "the mount reported {} rather than WebRTC on a relay-free endpoint",
                        selected
                            .as_deref()
                            .unwrap_or("no path before the settle deadline"),
                    ));
                }
                card.transport = data_path.borrow().clone();
            }
            match mesh::MeshPeer::join_share(resolved_mesh, shared, protocols, card).await {
                Ok(peer) => {
                    let peer = Rc::new(peer);
                    let install = {
                        let mut slot = mesh_slot.borrow_mut();
                        match &*slot {
                            MeshSlot::Left => false,
                            MeshSlot::Pending | MeshSlot::Joined(_) => {
                                *slot = MeshSlot::Joined(Rc::clone(&peer));
                                true
                            }
                        }
                    };
                    if install {
                        // The first manifest fetch may have beaten the join;
                        // publish the tree it recorded.
                        let tree = last_tree.borrow().clone();
                        if let Some(tree) = tree {
                            peer.set_tree(tree).await;
                        }
                    } else {
                        // Released mid-join (ticket change, retire): say
                        // goodbye instead of installing a leaked membership.
                        let _ = peer.leave().await;
                    }
                }
                Err(error) => {
                    web_sys::console::warn_1(&JsValue::from_str(&format!(
                        "[share] mesh unavailable; peer counts disabled: {error:?}"
                    )));
                }
            }
            // The origin answered, so this connect joined on the shared
            // endpoint — a membership left waiting by earlier dead-origin
            // attempts is now a duplicate identity and says goodbye, relay
            // registrations included: dropping it unclosed fed the ghost
            // roster every time a reconnecting tab won its origin race.
            //
            // Unless somebody is still living on it. A revival keeps the
            // dying client alive precisely so its serving half outlasts the
            // swap, and if that client came from the seeder lane it is homed
            // on *these* endpoints — closing them here would cut its peers'
            // downloads mid-stream and stop it serving before the
            // replacement re-arms. So the finding is recorded instead, and
            // `leave_mesh` collects the entry when its last holder lets go.
            let waiting = WAITING_MESHES.with(|meshes| {
                let mut meshes = meshes.borrow_mut();
                let key = share_mesh_key(&token);
                match meshes.get(&key) {
                    Some(entry) if !safe_to_retire(Rc::strong_count(&entry.peer)) => {
                        entry.duplicate.set(true);
                        None
                    }
                    _ => meshes.remove(&key),
                }
            });
            if let Some(waiting) = waiting {
                waiting.retire(true).await;
            }
        });
        // Prefetch the manifest so the page's first call skips its round
        // trip. Best-effort: a failure here surfaces on the real fetch, with
        // its password re-labelling intact.
        let prefetch_connection = client.connection.clone();
        let prefetch_cell = Rc::clone(&client.prefetched_manifest);
        let manifest_fetched = Rc::clone(&client.manifest_fetched);
        wasm_bindgen_futures::spawn_local(async move {
            if let Ok(pair) = fetch_manifest_on(&prefetch_connection, &token, author).await
                && !manifest_fetched.get()
            {
                *prefetch_cell.borrow_mut() = Some(pair);
            }
        });
        Ok(client)
    }

    /// Sync tech-info snapshot for the Info modal (counters + last-known IPs).
    ///
    /// Call [`Self::refresh_peer_ips`] on a slower cadence to fill ICE addresses;
    /// this getter never awaits `getStats`.
    #[must_use]
    #[wasm_bindgen]
    pub fn info(&self) -> JsValue {
        let json = self.info_json();
        js_sys::JSON::parse(&json.to_string()).unwrap_or(JsValue::NULL)
    }

    /// Refresh ICE remote-candidate addresses for live sessions.
    ///
    /// Both hubs, for the same reason [`Self::peers_direct`] counts both: the
    /// producer's session is in the mount hub, so sweeping only the mesh's
    /// would leave the producer's row showing no IP at all.
    ///
    /// # Errors
    /// Never fails today — reserved for future hard errors from getStats.
    #[wasm_bindgen]
    pub async fn refresh_peer_ips(&self) -> Result<(), JsValue> {
        let local_key = self._endpoint.id().to_string();
        let mut local_seen = false;
        // A peer can hold a session in *both* hubs (mount and mesh). Sampling
        // it twice in one sweep computes the second rate over a few
        // milliseconds with no byte delta, which overwrites the real rate with
        // zero — the counters climb while the UI insists nothing is moving.
        let mut sampled: HashSet<String> = HashSet::new();
        // Cloned out, not borrowed across the loop below: `hub()` hands back a
        // `&Arc`, and holding that reference would keep this `RefCell` borrowed
        // across every `await` in the sweep — which is the borrow `leave_mesh`
        // would then collide with, moving the panic rather than removing it.
        let mesh_hub = self.mesh_peer().map(|peer| Arc::clone(peer.hub()));
        let hubs = [mesh_hub, self._hub.clone()];
        for hub in hubs.into_iter().flatten() {
            for id in hub.live_peer_ids() {
                let key = id.to_string();
                if !sampled.insert(key.clone()) {
                    continue;
                }
                if let Some((ip, kind)) = hub.selected_remote_candidate(&id).await {
                    self.ip_cache
                        .borrow_mut()
                        .insert(key.clone(), split_candidate(ip, kind));
                }
                if let Some((sent, received, rtt_ms)) = link::read_ice(&hub, &id).await {
                    let now = now_ms();
                    let mut cache = self.bytes_cache.borrow_mut();
                    let previous = cache.get(&key).copied().unwrap_or_default();
                    cache.insert(key, previous.sample(sent, received, rtt_ms, now));
                }
                // Our own address, from the same selected pair. Any live
                // session answers it — they all run on this tab's ICE agent —
                // so the first one that does is enough.
                //
                // The `local_seen` flag rather than a `contains_key` in the
                // condition: a `RefCell` borrow taken inside an `&&` chain
                // lives across the `.await` that follows it, which is how a
                // single-threaded runtime earns an `already borrowed` panic
                // from code that reads as a plain short-circuit.
                if !local_seen && let Some((ip, kind)) = hub.selected_local_candidate(&id).await {
                    self.ip_cache
                        .borrow_mut()
                        .insert(local_key.clone(), split_candidate(ip, kind));
                    local_seen = true;
                }
            }
        }
        Ok(())
    }

    /// Sample the mount connection's wire counters and return totals and rates.
    ///
    /// **A sampler, not a getter** — hence the name. Rates come from
    /// differencing cumulative counters, so the caller's cadence *is* the
    /// averaging window, and a second call within the same tick computes a rate
    /// over a few milliseconds with no byte delta: it would overwrite the real
    /// rate with zero. Exactly one driver may call this. [`Self::refresh_peer_ips`]
    /// carries the same hazard for the same reason.
    ///
    /// Synchronous — the QUIC state machine answers without awaiting — and it
    /// answers on the **relay** path as well as the `WebRTC` one, which is the
    /// whole reason [`link`] exists. `getStats` cannot: there is no
    /// `RTCPeerConnection` on the relay.
    ///
    /// Shape: `{ total: {…}, lanes: [{ label, selected, … }] }`, each meter
    /// carrying `sent` / `received` / `up_bps` / `down_bps` / `rtt_ms`.
    #[must_use]
    #[wasm_bindgen]
    pub fn sample_link(&self) -> JsValue {
        let now = now_ms();
        let mut cache = self.link_cache.borrow_mut();
        let mut fold = |key: &str, sent: u64, received: u64, rtt_ms: Option<f64>, selected| {
            let previous = cache.get(key).copied().unwrap_or_default();
            #[expect(
                clippy::cast_precision_loss,
                reason = "wire byte counts stay far below 2^53; see link::Meter"
            )]
            let meter = previous
                .meter
                .sample(sent as f64, received as f64, rtt_ms, now);
            cache.insert(key.to_owned(), link::LaneMeter { meter, selected });
        };

        let (sent, received) = link::read_quic_total(&self.connection);
        fold(TOTAL_LANE, sent, received, None, false);
        for lane in link::read_quic(&self.connection) {
            fold(
                &lane.label,
                lane.sent,
                lane.received,
                lane.rtt_ms,
                lane.selected,
            );
        }

        // Rendered through the same function `info` uses, so a sampled reading
        // and a reported one cannot describe the same state differently.
        let json = link::to_json(&cache, TOTAL_LANE);
        js_sys::JSON::parse(&json.to_string()).unwrap_or(JsValue::NULL)
    }

    /// Members on this share's mesh, including us. `0` when the mesh is not up.
    #[must_use]
    #[wasm_bindgen(getter)]
    pub fn peers_gossip(&self) -> u32 {
        self.mesh_peer().map_or(0, |peer| peer.peers_gossip())
    }

    /// Peers we hold a direct `WebRTC` data channel with.
    ///
    /// The union of two hubs, not one. The mount rides a relay-free endpoint
    /// (that is what keeps its bytes off the relay) while the mesh rides the
    /// signal endpoint, so each owns its own hub and the producer's session
    /// lives only in the mount's. Reading the mesh hub alone would report `0
    /// direct` on a tab happily streaming files over a direct channel — the
    /// exact miscount the single-endpoint shape was meant to avoid.
    #[must_use]
    #[wasm_bindgen(getter)]
    pub fn peers_direct(&self) -> u32 {
        u32::try_from(self.direct_peer_ids().len()).unwrap_or(u32::MAX)
    }

    /// The direct-session ceiling this tab negotiates up to.
    #[must_use]
    #[wasm_bindgen(getter)]
    pub fn max_direct(&self) -> u32 {
        self.mesh_peer().map_or(0, |peer| peer.max_direct())
    }

    /// Leave the share's mesh, announcing departure so peers drop us now.
    ///
    /// Fire-and-forget, and deliberately not `async`: a `pagehide` handler
    /// cannot await.
    ///
    /// # This does not help on tab close — measured
    ///
    /// Closing a tab and watching a peer's roster for 35s: the member count
    /// does **not** drop. The spawned `Left` broadcast never gets to run,
    /// because the JS context is torn down before the microtask queue is
    /// drained. Departure still waits for the silence sweeper.
    ///
    /// It *is* effective on the in-page path — a ticket change unmounts the
    /// session while the page lives on, so the broadcast completes normally.
    /// That is the case this method actually earns its keep in.
    ///
    /// Making tab-close prompt needs a synchronous departure signal, which the
    /// engine does not currently have: something the browser can emit during
    /// unload (a `sendBeacon`-shaped path, or a relay-side hint), not an async
    /// gossip broadcast. Note the *direct* count is unaffected by any of this —
    /// the data channel closes immediately and `peers_direct` drops at once.
    /// Takes `&self`, not `&mut self`, and that is a hard requirement rather
    /// than a preference — see the note on the `mesh` field. A `&mut self`
    /// here panicked wasm-bindgen's borrow guard whenever a revival dropped
    /// this client while one of its async methods was still pending.
    pub fn leave_mesh(&self) {
        // `Pending` → `Left` covers a client released before its background
        // join landed: the join task observes `Left` and leaves the fresh
        // membership itself.
        let previous = std::mem::replace(&mut *self.mesh.borrow_mut(), MeshSlot::Left);
        let MeshSlot::Joined(peer) = previous else {
            return;
        };
        // A dead-origin client *shares* its membership with the waiting-mesh
        // registry, so revivals reuse the identity and every session it
        // built. Retiring such a client must only let go of its reference —
        // leaving here would disconnect the very membership its replacement
        // just graduated with. A membership only this client held (the
        // origin path) still says goodbye. [`Self::shutdown_mesh`] is the
        // unconditional farewell for a page that is actually going away.
        let shared = WAITING_MESHES.with(|meshes| {
            meshes
                .borrow()
                .get(&share_mesh_key(&self.token))
                .is_some_and(|waiting| Rc::ptr_eq(&waiting.peer, &peer))
        });
        if shared {
            // Letting go is the point, and it has to happen before the count
            // below can mean anything.
            drop(peer);
            // If an origin win already marked this membership a duplicate,
            // this client was the reason it could not be collected then.
            // Finish the job now, or the spare identity, its card and its
            // two relay registrations sit on the mesh until the page closes.
            // An *unmarked* membership is left alone on purpose: it is this
            // tab still seeding a share the user merely navigated away from.
            let collectable = WAITING_MESHES.with(|meshes| {
                let mut meshes = meshes.borrow_mut();
                let key = share_mesh_key(&self.token);
                match meshes.get(&key) {
                    Some(entry)
                        if collect_duplicate_now(
                            entry.duplicate.get(),
                            Rc::strong_count(&entry.peer),
                        ) =>
                    {
                        meshes.remove(&key)
                    }
                    _ => None,
                }
            });
            if let Some(waiting) = collectable {
                wasm_bindgen_futures::spawn_local(async move {
                    waiting.retire(true).await;
                });
            }
            return;
        }
        wasm_bindgen_futures::spawn_local(async move {
            let _ = peer.leave().await;
        });
    }

    /// Leave for real: purge the waiting-mesh registry and give up whatever
    /// can still be given up. The page-death path (`pagehide`) — nothing
    /// after this can reuse the membership, so nothing is kept.
    ///
    /// Be careful what you expect of the async half. This file has measured
    /// that spawned tasks do not run during page teardown, so the
    /// `peer.leave()` broadcast and the endpoint closes below are best
    /// effort that mostly does not happen: on a real tab close the browser
    /// tears the process down first, and this membership's relay
    /// registrations go on looking held until the far side times them out.
    /// Removing the registry entry and detaching the hubs is the part that
    /// is actually reliable, because both are synchronous, so both happen
    /// here rather than inside the task. Nothing downstream may be sized
    /// against a graceful departure that this path cannot promise — the
    /// ghost roster is a fact of tab-closing until the engine grows a
    /// synchronous departure signal.
    pub fn shutdown_mesh(&self) {
        let waiting =
            WAITING_MESHES.with(|meshes| meshes.borrow_mut().remove(&share_mesh_key(&self.token)));
        // Synchronous, so it survives a teardown that eats the task below.
        if let Some(waiting) = waiting.as_ref() {
            waiting.detach_hubs();
        }
        let previous = std::mem::replace(&mut *self.mesh.borrow_mut(), MeshSlot::Left);
        let own = match previous {
            MeshSlot::Joined(peer) => Some(peer),
            MeshSlot::Pending | MeshSlot::Left => None,
        };
        wasm_bindgen_futures::spawn_local(async move {
            // One goodbye per membership: a dead-origin client's own
            // membership *is* the waiting one, and its retire below carries
            // the leave.
            if let Some(peer) = &own
                && !waiting
                    .as_ref()
                    .is_some_and(|shared| Rc::ptr_eq(&shared.peer, peer))
            {
                let _ = peer.leave().await;
            }
            if let Some(waiting) = waiting {
                waiting.retire(true).await;
            }
        });
    }

    /// Which path carries mount data: `"webrtc"` or `"relay"`.
    #[must_use]
    #[wasm_bindgen(getter)]
    pub fn transport(&self) -> String {
        self.data_path.borrow().clone()
    }

    /// Has the mount connection gone away?
    ///
    /// A tab that is backgrounded loses it intermittently: the browser
    /// throttles timers — measured at 13–20 s intervals in Safari after about
    /// ten seconds hidden — and QUIC's keep-alive cannot outrun the idle
    /// timeout at that cadence. Nothing announces it, because [`Self::watch`]'s
    /// follower simply returns when the connection closes.
    ///
    /// So the page asks before it acts, and dials again when the answer is
    /// yes. Reconnecting is the only honest fix: a dead connection cannot be
    /// revived, and the alternative is a tab that looks connected and fails
    /// every action until it is reloaded.
    #[wasm_bindgen(getter)]
    pub fn closed(&self) -> bool {
        self.connection.close_reason().is_some()
    }

    /// Close the mount connection on purpose, so recovery can be exercised.
    ///
    /// Behind `?dev=true` in the Info pane. The failure this exists to
    /// rehearse is intermittent — a backgrounded tab loses its connection only
    /// sometimes — so before this the only way to test the reconnect path was
    /// to idle a tab for minutes and hope. Two such attempts produced no
    /// evidence either way.
    ///
    /// **It is not a reproduction of the bug.** The real death is silent:
    /// timers stretch past the keep-alive interval, packets stop arriving, and
    /// QUIC notices 30 s later. This closes the connection outright. What the
    /// two share is the state afterwards — [`Self::closed`] is true — which is
    /// all the recovery path keys on, and recovery is exactly what needs
    /// testing. Nothing here re-dials: that is left to the ordinary triggers,
    /// because a button that healed itself would bypass them.
    pub fn close_connection(&self) {
        self.connection
            .close(0u32.into(), b"dev: closed from the info pane");
    }

    /// The whole tree, in one shot: `{ dirs: [...], files: [...] }`.
    ///
    /// One request by design — the protocol has no per-directory listing op,
    /// so navigation is instant and only *bytes* are lazy.
    ///
    /// # Errors
    /// The producer refuses the request or the manifest does not decode.
    pub async fn manifest(&self) -> Result<JsValue, JsValue> {
        serde_wasm(&self.fetch_manifest().await?.manifest)
    }

    /// The manifest, and the exact bytes it was decoded from.
    ///
    /// The bytes matter separately from the struct: the tree fingerprint is
    /// taken over what the producer actually served, so both sides hash the
    /// same thing rather than trusting a re-encode to be canonical.
    async fn fetch_manifest(&self) -> Result<FetchedManifest, JsValue> {
        let prefetched = self.prefetched_manifest.borrow_mut().take();
        let fetched = match prefetched {
            // Already in hand — the seeder fallback's vetted pair, or the
            // origin prefetch. Skips a round trip, not any check below.
            Some(pair) => pair,
            None => match fetch_manifest_on(&self.connection, &self.token, self.author).await {
                Ok(pair) => pair,
                // The first request is where a refused credential surfaces: the
                // dial succeeds, and the producer only reads the token when the
                // stream header arrives. Re-label it so the page re-prompts for
                // a password instead of reporting a broken share.
                Err(error) if unauthorized_close(&self.connection) => {
                    return Err(unauthorized(&describe(&error)));
                }
                Err(error) => return Err(error),
            },
        };
        self.manifest_fetched.set(true);
        // Origin authority, enforced rather than assumed: a seeder was vetted
        // against one tree and answers for that tree only. Only the origin —
        // TLS-proven by dialing the ticket's endpoint id — may change it.
        if let Some(pinned) = self.pinned_tree.as_deref() {
            let fingerprint = agent_share_proto::manifest::manifest_fingerprint(&fetched.body);
            if fingerprint != pinned {
                return Err(JsValue::from_str(
                    "the seeder changed trees after being vetted; refusing its manifest",
                ));
            }
        }
        // The card could not carry a tree at join — the join runs in the
        // background, possibly after this — so record the fingerprint where
        // the join task can find it, and publish now if the mesh is up.
        //
        // Cloned out of the cell, not borrowed across the await below: see the
        // note on the `mesh` field.
        let fingerprint = agent_share_proto::manifest::manifest_fingerprint(&fetched.body);
        *self.last_tree.borrow_mut() = Some(fingerprint.clone());
        if let Some(mesh) = self.mesh_peer() {
            mesh.set_tree(fingerprint).await;
        }
        Ok(fetched)
    }

    /// Follow the share as it changes, calling `on_manifest` with the whole
    /// tree every time it does.
    ///
    /// The callback receives a complete manifest rather than a difference:
    /// deltas are how the *wire* stays small, but a UI wants the current
    /// state, and reassembling it here means the browser cannot drift from
    /// what the producer thinks it sent. Fire-and-forget — it runs until the
    /// connection ends, and a producer too old for the op simply never calls
    /// back.
    ///
    /// # Errors
    /// Never: the subscription runs in the background. Stream failures retry
    /// every 3s while the connection is alive; a clean zero-frame end means
    /// the producer does not support live watch and the loop stops.
    pub async fn watch(&self, on_manifest: js_sys::Function) -> Result<(), JsValue> {
        // `OP_WATCH` is only ever followed against the origin. A seeder serves
        // a frozen snapshot — it has no right to move the tree, and a watch
        // stream is exactly the channel a hostile one would use to try. The
        // manifest this client already fetched (vetted against the pinned
        // tree) is the share, unchanged for as long as the origin stays gone.
        if !self.from_origin {
            return Ok(());
        }
        let conn = self.connection.clone();
        let token = self.token;
        wasm_bindgen_futures::spawn_local(async move {
            loop {
                match follow_watch(&conn, &token, &on_manifest).await {
                    WatchEnd::Unsupported => return,
                    WatchEnd::Retryable => {
                        if conn.close_reason().is_some() {
                            return;
                        }
                        wait_ms(3_000).await;
                    }
                }
            }
        });
        Ok(())
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
            .map_err(|error| stream_open_failed("could not read the file", &error))?;
        send.write_all(&framing::encode_read_request(
            &self.token,
            index,
            offset,
            len,
        ))
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

    /// Fetch bytes into local storage so this tab can seed them.
    ///
    /// `only` names paths to take — a file, or a directory and everything under
    /// it. Omit it for the whole share. Everything already held is skipped, so
    /// calling this twice costs one manifest fetch.
    ///
    /// Returns `{ files, bytes, verified, unverified, skipped, held }`.
    ///
    /// # What "verified" means, and why unverified is not a failure
    ///
    /// The root comes from the **origin**, over the connection already
    /// authenticated to the ticket's endpoint id, and the bytes are hashed here
    /// and compared against it. A mismatch is fatal for that file: the bytes
    /// were altered in flight, or the origin is serving content it did not
    /// hash.
    ///
    /// An origin that cannot vouch for an index answers "no hash" rather than
    /// lying, and that is ordinary — it keeps the cache lazily. The copy stands
    /// and is seedable; it simply is not provable from here. Refusing to store
    /// it would make seeding depend on a cache the origin is free not to keep.
    ///
    /// # Errors
    /// The manifest cannot be fetched, storage is unavailable, or a file's
    /// bytes do not match the root the origin published.
    pub async fn sync(&self, only: Option<Vec<String>>) -> Result<JsValue, JsValue> {
        let FetchedManifest {
            envelope,
            body,
            manifest,
        } = self.fetch_manifest().await?;
        let store = self.open_store().await?;
        let only = only.unwrap_or_default();

        let mut files = 0u32;
        let mut total = 0u64;
        let mut verified = 0u32;
        let mut unverified = 0u32;
        let mut skipped = 0u32;

        for (index, entry) in manifest.files.iter().enumerate() {
            // A tombstone holds a slot open so later indices keep meaning what
            // they meant. There is nothing to fetch.
            if entry.is_tombstone() {
                continue;
            }
            if !wanted(&only, &entry.rel_path) {
                skipped += 1;
                continue;
            }
            let index = u32::try_from(index).map_err(|_| JsValue::from_str("index over u32"))?;

            // The row first. Without one this peer cannot address the file, and
            // the only honest fallback is a whole-file read from the origin.
            let Some(row) = self.row_for(index, store.as_ref()).await? else {
                let body = self.read_whole(index, entry.size).await?;
                let built = ChunkMap::build(&body);
                self.store_row_and_chunks(store.as_ref(), index, &built, 0, &body)
                    .await?;
                let _ = store.set_bind(&file_id(entry), built.root()).await;
                unverified += 1;
                files += 1;
                total += body.len() as u64;
                continue;
            };

            let before = store
                .coverage(row.root())
                .await
                .map_err(|error| err("reading coverage", &error))?;
            if before.is_complete() {
                skipped += 1;
                let _ = store.set_bind(&file_id(entry), row.root()).await;
                self.remember_row(index, &row);
                continue;
            }

            // Only the missing chunks, and spread over every peer that holds
            // them. Pressing Seed after a preview therefore costs the
            // difference, which is often nothing at all.
            let missing: Vec<usize> = before.missing().collect();
            total += self.fetch_missing(&row, index, &missing, store.as_ref()).await?;
            store
                .put_map(&row)
                .await
                .map_err(|error| err("storing a chunk row", &error))?;
            // The bind is what survives a reload: without it the store holds
            // chunks and rows but nothing maps this file to its root, so
            // `rows_in_store` finds nothing and the tab seeds none of what it
            // actually has.
            let _ = store.set_bind(&file_id(entry), row.root()).await;
            self.remember_row(index, &row);
            verified += 1;
            files += 1;
        }

        // The sidecar: what lets a refreshed tab stand this share back up
        // with no live source at all.
        persist_manifest(&self.token, store.as_ref(), &envelope).await;
        self.republish(&envelope, &body, &manifest, Arc::clone(&store))
            .await?;

        let out = serde_json::json!({
            "files": files,
            "bytes": total,
            "verified": verified,
            "unverified": unverified,
            "skipped": skipped,
            "held": self.card.borrow().held.len(),
        });
        js_sys::JSON::parse(&out.to_string())
    }

    /// Hand back bytes this tab already fetched, so it can seed them.
    ///
    /// **The point of the whole design.** A download, a ZIP of a folder and a
    /// preview all pull the same bytes through the same reader; without this
    /// they were thrown away and pressing Seed pulled them a second time.
    ///
    /// `offset` is where `bytes` sit in the file at `index`. Only chunks lying
    /// **wholly** inside the supplied range are kept — a partial chunk cannot
    /// be addressed, so it is dropped rather than stored under a guess. Feeding
    /// sequential 256 `KiB` pieces from the start therefore keeps every one of
    /// them, since that is exactly four aligned chunks.
    ///
    /// Cheap to call and safe to ignore: a failure here costs seeding, never
    /// the transfer that produced the bytes.
    ///
    /// # Errors
    /// Storage is unavailable. A verification mismatch is *not* an error — it
    /// means these bytes are not the file this tab thinks they are, and they
    /// are silently skipped rather than stored under an address they do not
    /// match.
    pub async fn keep(&self, index: u32, offset: u64, bytes: Vec<u8>) -> Result<(), JsValue> {
        let store = self.open_store().await?;
        let Some(row) = self.row_for(index, store.as_ref()).await? else {
            return Ok(());
        };
        self.store_row_and_chunks(store.as_ref(), index, &row, offset, &bytes)
            .await?;
        // Bind the row to this file version, or a reload would find a store
        // full of chunks and no way to tell which file they belong to.
        if let Ok(FetchedManifest { manifest, .. }) = self.fetch_manifest().await
            && let Some(entry) = manifest.files.get(index as usize)
        {
            let _ = store.set_bind(&file_id(entry), row.root()).await;
        }
        Ok(())
    }

    /// Publish what this tab now holds, in the order that keeps it honest.
    ///
    /// Serving before advertising: the seeder must be able to answer for a
    /// chunk by the time the card claims it, or a reader lands on `BadIndex`.
    pub async fn republish_holdings(&self) -> Result<(), JsValue> {
        let FetchedManifest {
            envelope,
            body,
            manifest,
        } = self.fetch_manifest().await?;
        let store = self.open_store().await?;
        self.republish(&envelope, &body, &manifest, store).await
    }

    /// What share of each known slot this tab holds, as `{ index: fraction }`.
    ///
    /// Fractions rather than flags, because a partial holding is now a real
    /// state that a peer can serve from: a cancelled download seeds what it
    /// got, and a view that showed nothing for it would understate the swarm
    /// exactly where it matters most.
    ///
    /// Only slots this tab has a row for appear. An absent slot is "we know
    /// nothing about that file", which is not the same as holding none of it.
    ///
    /// # Errors
    /// Storage is unavailable — which costs this view, never the share.
    pub async fn coverage_map(&self) -> Result<JsValue, JsValue> {
        let Ok(store) = self.open_store().await else {
            return js_sys::JSON::parse("{}");
        };
        let rows = self.rows.borrow().clone();
        let mut out = serde_json::Map::new();
        for (index, row) in &rows {
            let fraction = store
                .coverage(row.root())
                .await
                .map_or(0.0, |coverage| coverage.fraction());
            if let Some(number) = serde_json::Number::from_f64(fraction) {
                out.insert(index.to_string(), serde_json::Value::Number(number));
            }
        }
        js_sys::JSON::parse(&serde_json::Value::Object(out).to_string())
    }

    /// The row for `index`, from memory, then the store, then the peer.
    async fn row_for(&self, index: u32, store: &IdbStore) -> Result<Option<ChunkMap>, JsValue> {
        if let Some(row) = self.rows.borrow().get(&index) {
            return Ok(Some(row.clone()));
        }
        if let Some(row) = self.fetch_chunk_map(index).await? {
            let _ = store.put_map(&row).await;
            self.remember_row(index, &row);
            return Ok(Some(row));
        }
        Ok(None)
    }

    fn remember_row(&self, index: u32, row: &ChunkMap) {
        self.rows.borrow_mut().insert(index, row.clone());
    }

    /// Store whichever whole chunks of `row` lie inside `bytes` at `offset`.
    async fn store_row_and_chunks(
        &self,
        store: &IdbStore,
        index: u32,
        row: &ChunkMap,
        offset: u64,
        bytes: &[u8],
    ) -> Result<(), JsValue> {
        let end = offset.saturating_add(bytes.len() as u64);
        for position in 0..row.len() {
            let range = row.range_of(position);
            // Wholly inside, or not at all: half a chunk has no address.
            if range.start < offset || range.end > end {
                continue;
            }
            let from = usize::try_from(range.start - offset).unwrap_or(usize::MAX);
            let to = usize::try_from(range.end - offset).unwrap_or(usize::MAX);
            let Some(slice) = bytes.get(from..to) else {
                continue;
            };
            let Some(address) = row.leaf(position) else {
                continue;
            };
            // Bytes that do not address what the row says are not this file.
            // Skipped rather than stored, and not an error: the caller was
            // reading a file that changed underneath it.
            if chunk_hash(slice) != address {
                continue;
            }
            store
                .put(address, slice)
                .await
                .map_err(|error| err("storing a chunk", &error))?;
        }
        let _ = store.put_map(row).await;
        self.remember_row(index, row);
        Ok(())
    }

    /// One chunk, by address if the peer can do that, else by byte range.
    ///
    /// The fallback exists because a peer may serve `OP_READ` and nothing else
    /// — an older producer, or one with no chunk table. Bytes taken that way
    /// are still verified against the row before they are used.
    /// Dial up to [`SWARM_WIDTH`] other holders of this share, once.
    ///
    /// Best-effort throughout: a peer that will not answer is skipped, and a
    /// swarm that fails to form entirely leaves the transfer exactly as it was
    /// before this existed — one connection, which still works.
    ///
    /// Candidates come from the mesh roster rather than from any memory of past
    /// transfers, so a peer that arrived a moment ago is usable and one that
    /// left is not offered.
    ///
    /// # Take the channel before offering one
    ///
    /// Against another tab, a fresh JSEP round is refused outright: two tabs on
    /// one mesh already hold a data channel, and `produce::serve_signal` turns
    /// a second offer away with *"a WebRTC session with you already exists;
    /// dial the custom addr"*. So the custom addr is dialled first.
    ///
    /// It has to be tried on **both** of this tab's lanes. The remote's mount
    /// and mesh lanes share one hub, so whichever of ours reached it first owns
    /// the session and the other has nothing to route over — and which one that
    /// was is not knowable from here. Dialling only the mount lane looks
    /// correct, always failed, and fell through to a negotiation that could
    /// never be accepted.
    async fn dial_swarm(&self) {
        if self.swarm_dialled.replace(true) {
            return;
        }
        let Some(peer) = self.mesh_peer() else {
            return;
        };
        // Never ourselves, and never the peer we are already homed on — a
        // second connection to it would add a lane, not a source.
        let home = self.connection.remote_id().to_string();
        let own = peer.hub().local_id().to_string();
        let candidates: Vec<String> = peer
            .known_cards()
            .into_iter()
            .filter(|card| card.endpoint != home && card.endpoint != own)
            // A card with no `serving` holds nothing worth dialling for bulk.
            .filter(|card| card.serving.is_some())
            .map(|card| card.endpoint)
            .take(SWARM_WIDTH)
            .collect();
        if candidates.is_empty() {
            // Said out loud, once. A swarm that never forms is indistinguishable
            // from one that formed and helped, and the difference is the whole
            // feature — silence here is what made an early bug look like
            // ordinary single-peer behaviour.
            web_sys::console::log_1(&JsValue::from_str(
                "[share] swarm: no other holder on the mesh; pulling from the home peer alone",
            ));
            return;
        }
        // This tab's own lanes, not the waiting registry's: a client that
        // reached the origin consumed its waiting membership at connect, so
        // looking the entry up here found nothing and the swarm silently never
        // formed on the one path where it is most useful.
        let (Some(hub), Some(signal)) = (self._hub.as_ref(), self.signal_endpoint.as_ref()) else {
            // A relay-only mount has no data-channel hub to negotiate over.
            // Bulk still works; it just works from one peer.
            return;
        };
        let lanes = DialLanes {
            signal_endpoint: signal,
            mount_endpoint: &self._endpoint,
            mount_hub: hub,
        };
        // The relay ladder this tab is itself reachable on, which is the one
        // the ticket configured for everybody on this share — a JSEP round has
        // to meet somewhere, and that somewhere is the same for every peer.
        let relays: Vec<TransportAddr> = signal
            .addr()
            .relay_urls()
            .cloned()
            .map(TransportAddr::Relay)
            .collect();
        web_sys::console::debug_1(&JsValue::from_str(&format!(
            "[share] swarm: home={} self={} candidates={candidates:?}",
            &home[..8.min(home.len())],
            &own[..8.min(own.len())]
        )));
        for endpoint in candidates {
            let Ok(id) = endpoint.parse::<fofoca::protocol::iroh_base::EndpointId>() else {
                continue;
            };
            // Take the channel before offering one. Two tabs on one mesh
            // usually *already* have a data channel, and a peer in that state
            // refuses a fresh JSEP round outright: "a WebRTC session with you
            // already exists; dial the custom addr". Its own instruction, taken
            // literally.
            //
            // **Which lane holds that session is not knowable from here.** The
            // remote's mount lane and mesh lane share one hub — `serve_signal`
            // says so where it writes that refusal — so whichever of ours
            // reached the peer first owns the session, and the other has
            // nothing to route over. Trying only the mount lane is what made
            // this fall through to a JSEP round that could never be accepted.
            let channel = EndpointAddr::from_parts(id, [TransportAddr::Custom(custom_addr(id))]);
            let mut dialled = self
                ._endpoint
                .connect(channel.clone(), MOUNT_ALPN)
                .await
                .map_err(|error| err("dial a swarm peer over the mount lane", &error));
            if dialled.is_err() {
                dialled = signal
                    .connect(channel, MOUNT_ALPN)
                    .await
                    .map_err(|error| err("dial a swarm peer over the mesh lane", &error));
            }
            if dialled.is_err() {
                dialled =
                    seeder_webrtc_dial(lanes, id, &relays, f64::from(SEEDER_CHANNEL_WAIT_MS)).await;
            }
            match dialled {
                Ok(connection) => {
                    web_sys::console::log_1(&JsValue::from_str(&format!(
                        "[share] swarm: {} joined as a chunk source",
                        &endpoint[..8.min(endpoint.len())]
                    )));
                    self.swarm.borrow_mut().push(SwarmPeer {
                        connection,
                        endpoint,
                    });
                }
                Err(error) => {
                    web_sys::console::debug_1(&JsValue::from_str(&format!(
                        "[share] swarm: {} did not answer ({})",
                        &endpoint[..8.min(endpoint.len())],
                        describe(&error)
                    )));
                }
            }
        }
    }

    /// Fetch every missing chunk of `row`, spread across the swarm.
    ///
    /// The plan is pinned to `row.root()`: every peer answered `OP_HAVE` for
    /// that root, so a file edited mid-transfer produces a *different* root and
    /// a different download. This one finishes from peers still holding the old
    /// one or fails; it cannot mix, because each chunk is checked against the
    /// address the plan asked for.
    ///
    /// Returns the bytes fetched, for the tally.
    async fn fetch_missing(
        &self,
        row: &ChunkMap,
        index: u32,
        missing: &[usize],
        store: &IdbStore,
    ) -> Result<u64, JsValue> {
        if missing.is_empty() {
            return Ok(0);
        }
        web_sys::console::debug_1(&JsValue::from_str(&format!(
            "[share] swarm: slot {index} wants {} chunks",
            missing.len()
        )));
        self.dial_swarm().await;

        // Snapshotted, not borrowed: a `RefCell` borrow held across an await is
        // the panic this type is built to avoid — see the note on the `mesh`
        // field. A `Connection` is a handle, so cloning is cheap.
        let peers: Vec<(String, Connection)> = self
            .swarm
            .borrow()
            .iter()
            .map(|peer| (peer.endpoint.clone(), peer.connection.clone()))
            .collect();

        // Ask every extra peer what it holds of this root. Concurrently: the
        // answers are independent and a slow peer must not delay the plan.
        let coverages: Vec<Option<Coverage>> = futures::future::join_all(
            peers
                .iter()
                .map(|(_, conn)| fetch_have_on(conn, &self.token, row.root(), row.len())),
        )
        .await;
        let usable: Vec<(&(String, Connection), Coverage)> = peers
            .iter()
            .zip(coverages)
            .filter_map(|(peer, coverage)| Some((peer, coverage?)))
            .collect();

        let plan = swarm::plan(
            missing,
            &usable
                .iter()
                .map(|(_, coverage)| coverage.clone())
                .collect::<Vec<_>>(),
        );
        if !plan.peers.is_empty() {
            web_sys::console::log_1(&JsValue::from_str(&format!(
                "[share] swarm: {} chunks over {} peers, {} from the home peer",
                missing.len() - plan.unheld.len(),
                plan.peers.len(),
                plan.unheld.len()
            )));
        }

        let mut fetched = 0u64;
        // Each peer works its own queue; the queues run together. A peer that
        // fails mid-queue does not fail the download — its remaining positions
        // fall through to the home connection below, which is the simplest
        // form of the rescheduling BitTorrent calls endgame.
        let mut refused: Vec<usize> = Vec::new();
        let assigned = futures::future::join_all(plan.peers.iter().map(|peer_plan| {
            let (peer, _) = &usable[peer_plan.peer];
            self.drain_queue(peer, row, &peer_plan.positions)
        }))
        .await;
        for (bytes, missed) in assigned {
            for (address, chunk) in bytes {
                fetched += chunk.len() as u64;
                store
                    .put(address, &chunk)
                    .await
                    .map_err(|error| err("storing a chunk", &error))?;
            }
            refused.extend(missed);
        }

        // Whatever the swarm could not serve, from the peer we are homed on —
        // including every position no peer advertised at all.
        for position in plan.unheld.iter().copied().chain(refused) {
            let Some(address) = row.leaf(position) else {
                continue;
            };
            if store.has(address).await.unwrap_or(false) {
                continue;
            }
            let chunk = self
                .fetch_or_read_chunk(row, index, position, address)
                .await?;
            fetched += chunk.len() as u64;
            store
                .put(address, &chunk)
                .await
                .map_err(|error| err("storing a chunk", &error))?;
        }
        Ok(fetched)
    }

    /// Pull one peer's assigned positions in order.
    ///
    /// Returns what arrived and verified, plus the positions it could not
    /// serve. A peer that lies costs its own bandwidth and nothing else: the
    /// bytes are hashed here, and a mismatch is treated exactly as a refusal.
    async fn drain_queue(
        &self,
        peer: &(String, Connection),
        row: &ChunkMap,
        positions: &[usize],
    ) -> (Vec<(ChunkHash, Vec<u8>)>, Vec<usize>) {
        let (endpoint, connection) = peer;
        let mut got = Vec::new();
        let mut missed = Vec::new();
        for &position in positions {
            let Some(address) = row.leaf(position) else {
                continue;
            };
            match fetch_chunk_on(connection, &self.token, address).await {
                Ok(Some(bytes)) if chunk_hash(&bytes) == address => got.push((address, bytes)),
                Ok(_) => missed.push(position),
                Err(error) => {
                    web_sys::console::debug_1(&JsValue::from_str(&format!(
                        "[share] swarm: {} dropped out ({})",
                        &endpoint[..8.min(endpoint.len())],
                        describe(&error)
                    )));
                    // The rest of this queue goes with it: a connection that
                    // errored will not answer the next request either.
                    missed.push(position);
                    missed.extend(
                        positions
                            .iter()
                            .skip_while(|&&at| at != position)
                            .skip(1)
                            .copied(),
                    );
                    break;
                }
            }
        }
        (got, missed)
    }

    async fn fetch_or_read_chunk(
        &self,
        row: &ChunkMap,
        index: u32,
        position: usize,
        address: ChunkHash,
    ) -> Result<Vec<u8>, JsValue> {
        if let Some(bytes) = self.fetch_chunk(address).await?
            && chunk_hash(&bytes) == address
        {
            return Ok(bytes);
        }
        let range = row.range_of(position);
        let len = u32::try_from(range.end - range.start)
            .map_err(|_| JsValue::from_str("chunk length over u32"))?;
        let bytes = self.read_exact_range(index, range.start, len).await?;
        if chunk_hash(&bytes) != address {
            return Err(JsValue::from_str(
                "a peer answered with bytes that do not match the file's own addresses",
            ));
        }
        Ok(bytes)
    }

    /// Read exactly `len` bytes at `offset`, looping over the protocol cap.
    async fn read_exact_range(
        &self,
        index: u32,
        offset: u64,
        len: u32,
    ) -> Result<Vec<u8>, JsValue> {
        let mut out = Vec::with_capacity(len as usize);
        while (out.len() as u32) < len {
            let want = len - out.len() as u32;
            let piece = self.read(index, offset + out.len() as u64, want).await?;
            if piece.is_empty() {
                return Err(JsValue::from_str(
                    "the peer stopped short of the size the manifest describes",
                ));
            }
            out.extend_from_slice(&piece);
        }
        Ok(out)
    }

    /// Adopt what this tab holds into the seeder, then advertise it.
    ///
    /// The two byte strings are not interchangeable and the compiler cannot say
    /// so: `envelope` is the whole `OP_MANIFEST` body that the seeder re-serves,
    /// `body` is the bare manifest inside it, which is the only thing a
    /// fingerprint may be taken over. [`FetchedManifest`] carries both, named.
    async fn republish(
        &self,
        envelope: &[u8],
        body: &[u8],
        manifest: &MountManifest,
        store: Arc<IdbStore>,
    ) -> Result<(), JsValue> {
        let rows = self.rows.borrow().clone();
        // Serving before advertising: the seeder must answer for a chunk by the
        // time the card claims it, or a reader lands on `BadIndex`.
        self.seeder
            .update(Arc::new(envelope.to_vec()), rows, store);
        // Asked of the seeder rather than walked here, so this and the
        // retraction below cannot disagree about what "fully held" means — two
        // rules would flap, each undoing the other's card.
        *self.card.borrow_mut() = CardHoldings {
            held: self.seeder.complete_slots().await.into_iter().collect(),
            slots: manifest.files.len(),
        };
        self.publish_serving(body).await;
        // Started here rather than at connect: this is the first moment the card
        // promises anything, and by now the seeder is final — the dead-origin
        // path swaps in the waiting mesh's seeder while connecting.
        if !self.retraction_watched.replace(true) {
            wasm_bindgen_futures::spawn_local(watch_for_lost_holdings(
                self.seeder.downgrade(),
                Rc::clone(&self.mesh),
                Rc::clone(&self.card),
            ));
        }
        Ok(())
    }

    /// Manifest indices this tab holds in full, and can seed.
    #[must_use]
    #[wasm_bindgen(getter)]
    pub fn held(&self) -> Vec<u32> {
        self.card.borrow().held.iter().copied().collect()
    }

    /// Whether the mount reaches the ticket's origin, or a seeder standing in
    /// for it. The UI keys authority-sensitive behaviour off this: a mirror
    /// sync from a seeder must treat a short read as a failure, never as EOF.
    #[must_use]
    #[wasm_bindgen(getter)]
    pub fn source_is_origin(&self) -> bool {
        self.from_origin
    }

    /// Recompute what is held from storage, and republish it.
    ///
    /// Called on mount so a reload shows what survived rather than an empty
    /// grid. Never fails: a tab with no store simply holds nothing.
    pub async fn refresh_held(&self) -> Result<(), JsValue> {
        let Ok(FetchedManifest {
            envelope,
            body,
            manifest,
        }) = self.fetch_manifest().await
        else {
            return Ok(());
        };
        // Deliberately does *not* create a database — only adopts one already
        // there. Browsing a share must not leave storage behind.
        let Ok(Some(store)) = IdbStore::adopt(&self.store_name()).await else {
            return Ok(());
        };
        let store = Arc::new(store);
        // Rows come from the store, never from an in-memory tally: a tally that
        // lost an entry to the reload would make this tab claim less than it
        // has, and would make a later sweep delete more than it should.
        let rows = rows_in_store(store.as_ref(), &manifest).await;
        *self.rows.borrow_mut() = rows;
        *self.store.borrow_mut() = Some(Arc::clone(&store));
        // A tab that seeded in an earlier session re-persists the (possibly
        // newer) manifest on its next healthy visit, keeping the sidecar fresh
        // for the next resurrection.
        if !self.rows.borrow().is_empty() {
            persist_manifest(&self.token, store.as_ref(), &envelope).await;
        }
        self.republish(&envelope, &body, &manifest, store).await
    }

    /// Where this share's blocks live. See [`store_name_for`].
    fn store_name(&self) -> String {
        store_name_for(&self.token)
    }

    async fn open_store(&self) -> Result<Arc<IdbStore>, JsValue> {
        if let Some(store) = self.store.borrow().as_ref() {
            return Ok(Arc::clone(store));
        }
        let store = Arc::new(
            IdbStore::open(&self.store_name())
                .await
                .map_err(|error| err("opening local storage", &error))?,
        );
        *self.store.borrow_mut() = Some(Arc::clone(&store));
        Ok(store)
    }

    /// Tell the mesh which slots this tab can serve.
    ///
    /// Both fields together: an index means nothing without agreeing which
    /// manifest it indexes into, so a `serving` set published against the wrong
    /// tree would send readers to the wrong files.
    ///
    /// `body` is the **bare manifest**, which is the domain `card.tree` is
    /// defined over — never the envelope around it. Callers hold both and it is
    /// the envelope that is closer to hand, which is how this once came to
    /// publish a tree fingerprint no other peer computes: signing split the two
    /// and the caller kept passing the one it had.
    async fn publish_serving(&self, body: &[u8]) {
        let fingerprint = agent_share_proto::manifest::manifest_fingerprint(body);
        *self.last_tree.borrow_mut() = Some(fingerprint.clone());
        // Cloned out of the cell, not borrowed across the two awaits below:
        // see the note on the `mesh` field.
        let Some(mesh) = self.mesh_peer() else {
            return;
        };
        mesh.set_tree(fingerprint).await;
        let serving = self.card.borrow().serving();
        mesh.set_serving(serving).await;
    }

    /// Read a whole file, in protocol-sized pieces.
    async fn read_whole(&self, index: u32, size: u64) -> Result<Vec<u8>, JsValue> {
        let mut out = Vec::new();
        while (out.len() as u64) < size {
            let remaining = size - out.len() as u64;
            let want = u32::try_from(remaining.min(u64::from(MAX_READ_LEN)))
                .map_err(|_| JsValue::from_str("read length over u32"))?;
            let piece = self.read(index, out.len() as u64, want).await?;
            if piece.is_empty() {
                // Never treat a short answer as the end of the file: a caller
                // cannot tell truncation from a small file, and a silently
                // truncated mirror is the failure this whole design exists to
                // avoid.
                return Err(JsValue::from_str(
                    "the peer stopped short of the size the manifest describes",
                ));
            }
            out.extend_from_slice(&piece);
        }
        Ok(out)
    }

    /// The chunk row a peer publishes for `index`, if it can address one.
    ///
    /// `None` is ordinary: a peer with no chunk table, an index out of range,
    /// or a file that moved. All three mean "fall back to reading bytes", which
    /// is what the caller does.
    async fn fetch_chunk_map(&self, index: u32) -> Result<Option<ChunkMap>, JsValue> {
        let (mut send, mut recv) = self
            .connection
            .open_bi()
            .await
            .map_err(|error| err("open chunk map stream", &error))?;
        send.write_all(&framing::encode_chunk_map_request(&self.token, index))
            .await
            .map_err(|error| err("send chunk map request", &error))?;
        send.finish().map_err(|error| err("finish", &error))?;

        let mut status = [0u8; 1];
        if recv.read_exact(&mut status).await.is_err() {
            // A stream closed unanswered is a peer that predates the op. Reads
            // as "cannot address", which every caller already handles, rather
            // than as a failure that would abort the whole sync.
            return Ok(None);
        }
        if status[0] != agent_share_proto::manifest::ReadStatus::Ok.to_byte() {
            return Ok(None);
        }
        let len = read_len(&mut recv, framing::MAX_CHUNK_MAP_BYTES).await?;
        let mut body = vec![0u8; len as usize];
        recv.read_exact(&mut body)
            .await
            .map_err(|error| err("read chunk map", &error))?;
        let (root, size, addresses) = framing::decode_chunk_map(&body)
            .map_err(|error| JsValue::from_str(&format!("{error}")))?;
        let leaves: Vec<ChunkHash> = addresses.into_iter().map(ChunkHash::from_bytes).collect();
        // The root is recomputed from the row rather than taken on trust, so a
        // peer whose row and root disagree is caught here instead of at every
        // later verification.
        let row = ChunkMap::from_leaves(leaves, size)
            .map_err(|error| JsValue::from_str(&format!("{error}")))?;
        if row.root().as_bytes() != &root {
            return Err(JsValue::from_str(
                "a peer sent a chunk row whose root does not match its own addresses",
            ));
        }
        Ok(Some(row))
    }

    /// One chunk from the peer this tab is homed on, by address alone.
    async fn fetch_chunk(&self, address: ChunkHash) -> Result<Option<Vec<u8>>, JsValue> {
        fetch_chunk_on(&self.connection, &self.token, address).await
    }

    /// Connect using the transport encoded in the ticket and measure for
    /// `duration_secs` (default 30s after connect).
    ///
    /// `on_status` is called with a status object
    /// `{ stage: "connecting"|"connected"|"benching"|"progress", … }` when set.
    ///
    /// # Errors
    /// Ticket lacks a bench transport flag, connect failure, or producer
    /// lacks `OP_BENCH`.
    pub async fn bench(
        ticket: String,
        duration_secs: Option<u64>,
        on_status: Option<js_sys::Function>,
    ) -> Result<JsValue, JsValue> {
        console_error_panic_hook::set_once();
        let ticket = MountTicket::decode(&ticket).map_err(|error| err("decode ticket", &error))?;
        let transport_label = match ticket.kind {
            TICKET_KIND_BENCH_RELAY => "relay",
            TICKET_KIND_BENCH_WEBRTC => "webrtc",
            other => {
                return Err(JsValue::from_str(&format!(
                    "ticket has no bench transport (kind={other}); produce with --transport webrtc|relay"
                )));
            }
        };
        emit_status(
            on_status.as_ref(),
            &serde_json::json!({ "stage": "connecting", "transport": transport_label }),
        );

        let connect_start = now_ms();
        let client = match ticket.kind {
            // Bench relay must not fall through to direct IP (same-machine
            // benches were reporting ~localhost numbers labeled "relay").
            TICKET_KIND_BENCH_RELAY => {
                let token = ticket.secret;
                connect_relay_only(ticket, token).await?
            }
            TICKET_KIND_BENCH_WEBRTC => {
                let token = ticket.secret;
                connect_webrtc(ticket, token, /*allow_relay_fallback=*/ false).await?
            }
            _ => unreachable!("validated above"),
        };
        let connect_ms = now_ms() - connect_start;
        emit_status(
            on_status.as_ref(),
            &serde_json::json!({
                "stage": "connected",
                "transport": client.data_path.borrow().clone(),
                "connect_ms": connect_ms,
            }),
        );

        let duration_secs = duration_secs.unwrap_or(DEFAULT_BENCH_DURATION_SECS).max(1);
        let duration_ms = (duration_secs as f64) * 1000.0;
        emit_status(
            on_status.as_ref(),
            &serde_json::json!({ "stage": "benching", "duration_s": duration_secs }),
        );
        let measured = measure_window(&client, duration_ms, on_status.as_ref()).await?;

        let report = BenchReport {
            transport: client.data_path.borrow().clone(),
            connect_ms,
            duration_s: measured.duration_s,
            latency_ms: measured.latency_ms,
            throughput_mib_s: measured.throughput_mib_s,
            bytes: measured.bytes,
            pings: measured.pings,
        };
        client.connection.close(0u32.into(), b"bench done");
        client._endpoint.close().await;
        serde_wasm(&report)
    }
}

impl ShareClient {
    /// The mesh peer, when joined. `Pending`/`Left` read as "no mesh", the
    /// same answer `None` used to give.
    fn mesh_peer(&self) -> Option<Rc<mesh::MeshPeer>> {
        match &*self.mesh.borrow() {
            MeshSlot::Joined(peer) => Some(Rc::clone(peer)),
            MeshSlot::Pending | MeshSlot::Left => None,
        }
    }

    fn info_json(&self) -> serde_json::Value {
        let producer = self.connection.remote_id().to_string();
        let local = self._endpoint.id().to_string();
        let mesh = self.mesh_peer();
        let mesh_up = mesh.is_some();
        let nickname = mesh.as_ref().map(|m| m.nickname());
        let peers_gossip = self.peers_gossip();
        let peers_direct = self.peers_direct();
        let max_direct = self.max_direct();
        // The *rendezvous* endpoint's relays, not the mount's. On the WebRTC
        // path the mount endpoint is deliberately relay-free, so reading it
        // reports "no live relay URLs" on a tab that is very much talking to
        // one — the relay still carries rendezvous and gossip, just not file
        // bytes.
        // Live, not the connect-time snapshot: iroh re-selects when a path
        // opens or is abandoned, and a pane that froze its answer would keep
        // asserting a path the connection had already left.
        let live_path = selected_path_label(&self.connection)
            .unwrap_or_else(|| self.data_path.borrow().clone());
        let relay_urls: Vec<String> = if self.rendezvous_relays.is_empty() {
            self._endpoint
                .addr()
                .relay_urls()
                .map(|url| url.to_string())
                .collect()
        } else {
            self.rendezvous_relays.clone()
        };
        let producer_reach = serde_json::json!({
            "mdns": self.lookups.mdns,
            "dht": self.lookups.dht,
            "relay": match &self.lookups.relay {
                RelayChoice::Disabled => "disabled",
                RelayChoice::Pinned => "pinned",
                RelayChoice::Custom(_) => "custom",
            },
        });
        let peers = self.swarm_peers_json(&local, &producer);
        serde_json::json!({
            "general": {
                "transport": live_path,
                "identity_fingerprint": identity_fingerprint(&self.token),
                "mesh_up": mesh_up,
                "nickname": nickname,
                "local_endpoint": local,
                "producer_endpoint": producer,
                "connected_ms_ui": (now_ms() - self.connected_at_ms).max(0.0),
            },
            "trackers": {
                "relay_urls": relay_urls,
                "producer_reach": producer_reach,
            },
            "swarm": {
                "peers_gossip": peers_gossip,
                "peers_direct": peers_direct,
                "max_direct": max_direct,
                "peers": peers,
            },
            "transfer": {
                "mount_mode": self.mount_mode,
                "mount_path": live_path,
                "mount_paths": path_labels(&self.connection),
                "mount_fallback_reason": self.fallback_reason.borrow().clone(),
                "link": self.link_snapshot(),
            },
        })
    }

    /// The last [`Self::sample_link`] reading, **without taking a new one**.
    ///
    /// `info` is documented as a getter that never samples, and it has to stay
    /// one: the Info pane and the status bar read on the same tick, so a
    /// sampling `info` would difference over a few milliseconds and zero the
    /// rates the bar had just computed. Empty until the driver has sampled once.
    fn link_snapshot(&self) -> serde_json::Value {
        link::to_json(&self.link_cache.borrow(), TOTAL_LANE)
    }

    /// Endpoint ids we hold a live data channel with, across both hubs.
    ///
    /// Deduplicated: a mesh peer that is also the producer would otherwise be
    /// counted twice, since it can appear in either hub.
    fn direct_peer_ids(&self) -> HashSet<String> {
        let mut ids: HashSet<String> = self
            .mesh_peer()
            .map(|mesh| {
                mesh.hub()
                    .live_peer_ids()
                    .into_iter()
                    .map(|id| id.to_string())
                    .collect()
            })
            .unwrap_or_default();
        if let Some(hub) = self._hub.as_ref() {
            ids.extend(hub.live_peer_ids().into_iter().map(|id| id.to_string()));
        }
        ids
    }

    /// Every endpoint id that is *this tab*.
    ///
    /// Usually one. On the seeder path it is two: `connect_via_seeder` mints a
    /// separate mount key so a seeder's one-registry rule sees a fresh peer
    /// rather than the mesh lane's session, and the meta card is published
    /// under the *mesh* key while `info_json`'s `local` is the *mount* key.
    /// Both are us, so both have to be excluded from the mesh rows and both
    /// have to be searched for our own card — reading either alone listed this
    /// tab twice, once as `self` and once as a stranger on gossip.
    fn self_endpoint_ids(&self) -> HashSet<String> {
        let mut ids = HashSet::new();
        ids.insert(self._endpoint.id().to_string());
        if let Some(mesh) = self.mesh_peer() {
            ids.insert(mesh.hub().local_id().to_string());
        }
        ids
    }

    fn swarm_peers_json(&self, local: &str, producer: &str) -> Vec<serde_json::Value> {
        let mesh = self.mesh_peer();
        let (present, all) = mesh
            .as_ref()
            .map(|mesh| (mesh.known_cards(), mesh.all_cards()))
            .unwrap_or_default();
        swarm_rows(
            &SwarmView {
                local,
                ours: &self.self_endpoint_ids(),
                producer,
                direct: &self.direct_peer_ids(),
                present: &present,
                all: &all,
                data_path: &self.data_path.borrow(),
            },
            &self.ip_cache.borrow(),
            &self.bytes_cache.borrow(),
        )
    }
}

/// Everything the Peers list is built from, gathered off the live client.
///
/// A struct rather than nine arguments, and not only for the count: four of
/// them are endpoint ids, which as positional `&str`s would be swappable in
/// silence. Bundling them is also what makes [`swarm_rows`] a free function
/// testable without a live `Connection` — the reason the row assembly moved
/// out of `ShareClient` at all.
struct SwarmView<'a> {
    /// Our mount identity: the id the `self` row carries, and the key both
    /// caches are on.
    local: &'a str,
    /// Every id that is us. See [`ShareClient::self_endpoint_ids`].
    ours: &'a HashSet<String>,
    producer: &'a str,
    /// Peers we hold a live data channel with, across both hubs.
    direct: &'a HashSet<String>,
    /// Cards of the peers the mesh says are here — the gossip rows.
    present: &'a [PeerCard],
    /// Every card the document holds, for decorating a row that exists
    /// whether or not the mesh still lists its peer.
    all: &'a [PeerCard],
    data_path: &'a str,
}

/// Assemble one row per peer, each from exactly one source.
///
/// The order is the precedence: ourselves, the producer, anyone we hold a
/// channel to, then the rest of the mesh. Rows above the mesh do not come from
/// the roster and so cannot vanish with it — a producer we are streaming from
/// stays listed even if gossip has gone quiet entirely.
fn swarm_rows(
    view: &SwarmView<'_>,
    ip: &HashMap<String, (Option<String>, Option<String>)>,
    bytes: &HashMap<String, link::Meter>,
) -> Vec<serde_json::Value> {
    let card_for = |id: &str| view.all.iter().find(|card| card.endpoint == id).cloned();
    // Our own card is published under whichever identity joined the mesh, and
    // on the seeder path that is not the one the row is keyed by.
    let own_card = || {
        let mut ours: Vec<&String> = view.ours.iter().collect();
        ours.sort();
        card_for(view.local).or_else(|| ours.into_iter().find_map(|id| card_for(id)))
    };
    // Only reached before the tab has published anything of its own.
    let self_label = PeerCard::new(
        view.local,
        env!("CARGO_PKG_VERSION"),
        "browser",
        view.data_path,
        Some("consumer".to_owned()),
    )
    .client;
    let peer_row = |id: &str,
                    role: &str,
                    flags: String,
                    fallback_proto: &str,
                    ip: Option<String>,
                    ip_kind: Option<String>| {
        let card = if role == "self" {
            own_card()
        } else {
            card_for(id)
        };
        let client = card
            .as_ref()
            .map(|card| card.client.clone())
            .unwrap_or_else(|| {
                if id == view.local {
                    self_label.clone()
                } else {
                    "unknown".to_owned()
                }
            });
        let proto = card
            .as_ref()
            .map(|card| card.transport.clone())
            .filter(|transport| !transport.is_empty())
            .unwrap_or_else(|| fallback_proto.to_owned());
        let version = card.as_ref().map(|card| card.version.clone());
        let runtime = card.as_ref().map(|card| card.runtime.clone());
        let app_role = card.as_ref().and_then(|card| card.role.clone());
        // Availability, for the grid. `tree` rides along because a slot
        // index means nothing without agreeing which manifest it indexes
        // into — two peers on different trees must not be drawn as though
        // their squares line up.
        let serving = card.as_ref().and_then(|card| card.serving.clone());
        let tree = card.as_ref().and_then(|card| card.tree.clone());
        let stats = bytes.get(id).copied().unwrap_or_default();
        serde_json::json!({
            "id": id,
            "role": role,
            "bytes_sent": stats.sent,
            "bytes_received": stats.received,
            "up_bps": stats.up_bps,
            "down_bps": stats.down_bps,
            "rtt_ms": stats.rtt_ms,
            "flags": flags,
            "client": client,
            "version": version,
            "runtime": runtime,
            "app_role": app_role,
            "ip": ip,
            "ip_kind": ip_kind,
            "proto": proto,
            "serving": serving,
            "tree": tree,
        })
    };

    let mut rows = Vec::new();
    let (local_ip, local_ip_kind) = ip.get(view.local).cloned().unwrap_or((None, None));
    rows.push(peer_row(
        view.local,
        "self",
        "*".to_owned(),
        view.data_path,
        local_ip,
        local_ip_kind,
    ));

    // Both hubs: the producer's session lives in the mount's, every other
    // direct peer in the mesh's. Reading one would drop the `D` flag off
    // whichever half it missed.
    let (producer_ip, producer_ip_kind) = ip.get(view.producer).cloned().unwrap_or((None, None));
    let mut flags = String::from("S");
    if view.direct.contains(view.producer) {
        flags.push('D');
    }
    rows.push(peer_row(
        view.producer,
        "producer",
        flags,
        view.data_path,
        producer_ip,
        producer_ip_kind,
    ));

    let mut seen: HashSet<&str> = view.ours.iter().map(String::as_str).collect();
    seen.insert(view.local);
    seen.insert(view.producer);

    let mut direct: Vec<&str> = view.direct.iter().map(String::as_str).collect();
    direct.sort_unstable();
    for id in direct {
        if !seen.insert(id) {
            continue;
        }
        let (ip, ip_kind) = ip.get(id).cloned().unwrap_or((None, None));
        rows.push(peer_row(
            id,
            "direct",
            "D".to_owned(),
            "webrtc",
            ip,
            ip_kind,
        ));
    }

    // Mesh members publish meta cards but may never open a direct hub session
    // — still show them, so the Peers list matches the mesh count.
    for card in view.present {
        if !seen.insert(card.endpoint.as_str()) {
            continue;
        }
        let fallback = if card.transport.is_empty() {
            "gossip"
        } else {
            card.transport.as_str()
        };
        rows.push(peer_row(
            &card.endpoint,
            "gossip",
            String::new(),
            fallback,
            None,
            None,
        ));
    }

    rows
}

fn identity_fingerprint(token: &[u8; SECRET_LEN]) -> String {
    let key = share_mesh_key(token);
    let head = key.get(..8).unwrap_or(&key);
    let tail = key.get(key.len().saturating_sub(8)..).unwrap_or("");
    format!("{head}…{tail}")
}

/// Split a getStats candidate into the cache's `(address, kind)` shape.
///
/// Chrome blanks the **local** candidate's `address`/`ip` — deliberately, for
/// the same privacy reason host candidates are mDNS names. The candidate *type*
/// survives, and it is the more useful half anyway: `host` versus `srflx`
/// versus `relay` is what says whether a peer is direct. So an empty address
/// becomes `None` rather than an empty string the UI would render as a value.
fn split_candidate(address: String, kind: String) -> (Option<String>, Option<String>) {
    let address = (!address.trim().is_empty()).then_some(address);
    let kind = (!kind.trim().is_empty()).then_some(kind);
    (address, kind)
}

/// The path carrying bytes **right now**, or `None` before selection settles.
///
/// `data_path` is a snapshot taken once, seconds after connect. iroh can
/// re-select later — a path opening or being abandoned re-runs selection — so a
/// stored string is a claim about the past presented as the present. The info
/// pane asks this instead, and only falls back to the stored value while
/// nothing is selected yet.
fn selected_path_label(connection: &Connection) -> Option<String> {
    connection
        .paths()
        .iter()
        .find(|path| path.is_selected())
        .map(|path| link::path_label(path.remote_addr()))
}

fn path_labels(connection: &Connection) -> Vec<String> {
    connection
        .paths()
        .iter()
        .map(|path| link::path_label(path.remote_addr()))
        .collect()
}

fn emit_status(on_status: Option<&js_sys::Function>, value: &serde_json::Value) {
    let Some(callback) = on_status else {
        return;
    };
    let Ok(js) = serde_wasm(value) else {
        return;
    };
    let _ = callback.call1(&JsValue::NULL, &js);
}

#[derive(serde::Serialize)]
struct LatencyStats {
    min: f64,
    median: f64,
    p95: f64,
}

#[derive(serde::Serialize)]
struct BenchReport {
    transport: String,
    connect_ms: f64,
    duration_s: f64,
    latency_ms: LatencyStats,
    throughput_mib_s: f64,
    bytes: u64,
    pings: u32,
}

struct WindowStats {
    duration_s: f64,
    latency_ms: LatencyStats,
    throughput_mib_s: f64,
    bytes: u64,
    pings: u32,
}

fn now_ms() -> f64 {
    js_sys::Date::now()
}

async fn measure_window(
    client: &ShareClient,
    duration_ms: f64,
    on_status: Option<&js_sys::Function>,
) -> Result<WindowStats, JsValue> {
    let start = now_ms();
    let deadline = start + duration_ms;
    let total_secs = (duration_ms / 1000.0).round().max(1.0) as u64;
    let echo_every_ms = (BENCH_ECHO_INTERVAL_SECS.max(1) as f64) * 1000.0;
    let tick_every_ms = 5_000.0;
    let mut next_echo = start;
    let mut next_tick = start + tick_every_ms;
    let mut samples = Vec::new();
    let mut transferred = 0u64;

    while now_ms() < deadline {
        let now = now_ms();
        if on_status.is_some() && now >= next_tick {
            let elapsed = ((now - start) / 1000.0).floor().max(0.0) as u64;
            let elapsed = elapsed.min(total_secs);
            emit_status(
                on_status,
                &serde_json::json!({
                    "stage": "progress",
                    "elapsed_s": elapsed,
                    "duration_s": total_secs,
                }),
            );
            next_tick = now + tick_every_ms;
        }
        if now_ms() >= next_echo {
            samples.push(echo_once(client).await?);
            next_echo = now_ms() + echo_every_ms;
        } else {
            transferred += fill_once(client, MAX_BENCH_FILL_BYTES).await?;
        }
    }
    if samples.is_empty() {
        samples.push(echo_once(client).await?);
    }

    let duration_s = ((now_ms() - start) / 1000.0).max(1e-9);
    let pings = u32::try_from(samples.len()).unwrap_or(u32::MAX);
    #[expect(
        clippy::cast_precision_loss,
        reason = "throughput display only; byte counts stay exact in `bytes`"
    )]
    let mib_s = (transferred as f64) / (1024.0 * 1024.0) / duration_s;
    Ok(WindowStats {
        duration_s,
        latency_ms: latency_stats(&mut samples),
        throughput_mib_s: mib_s,
        bytes: transferred,
        pings,
    })
}

async fn echo_once(client: &ShareClient) -> Result<f64, JsValue> {
    let payload = [0xABu8; 32];
    let request = framing::encode_bench_echo_request(&client.token, &payload)
        .map_err(|error| err("encode echo", &error))?;
    let start = now_ms();
    let (mut send, mut recv) = client
        .connection
        .open_bi()
        .await
        .map_err(|error| stream_open_failed("bench echo", &error))?;
    send.write_all(&request)
        .await
        .map_err(|error| err("send echo", &error))?;
    send.finish().map_err(|error| err("finish echo", &error))?;
    let len = read_header(&mut recv, MAX_BENCH_ECHO_BYTES).await?;
    let mut body = vec![0u8; len as usize];
    recv.read_exact(&mut body)
        .await
        .map_err(|error| err("read echo", &error))?;
    if body != payload {
        return Err(JsValue::from_str("echo payload mismatch"));
    }
    Ok(now_ms() - start)
}

async fn fill_once(client: &ShareClient, want: u32) -> Result<u64, JsValue> {
    let request = framing::encode_bench_fill_request(&client.token, want)
        .map_err(|error| err("encode fill", &error))?;
    let (mut send, mut recv) = client
        .connection
        .open_bi()
        .await
        .map_err(|error| stream_open_failed("bench fill", &error))?;
    send.write_all(&request)
        .await
        .map_err(|error| err("send fill", &error))?;
    send.finish().map_err(|error| err("finish fill", &error))?;
    let len = read_header(&mut recv, want).await?;
    let mut left = len as usize;
    let mut buf = vec![0u8; 64 * 1024];
    while left > 0 {
        let take = left.min(buf.len());
        recv.read_exact(&mut buf[..take])
            .await
            .map_err(|error| err("read fill", &error))?;
        left -= take;
    }
    Ok(u64::from(len))
}

fn latency_stats(samples: &mut [f64]) -> LatencyStats {
    samples.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    let min = samples.first().copied().unwrap_or(0.0);
    LatencyStats {
        min,
        median: percentile(samples, 0.50),
        p95: percentile(samples, 0.95),
    }
}

fn percentile(sorted: &[f64], fraction: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "index into a small sample vec; fraction is in 0..=1"
    )]
    let idx = ((sorted.len() as f64 - 1.0) * fraction).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// One chunk from `conn`, by address alone.
///
/// `None` means "I do not hold that", which is how a peer holding part of a
/// file declines the parts it lacks. The bytes are **not** trusted here — the
/// caller checks them against the address it asked for, which is what lets a
/// chunk be taken from a peer nobody has any reason to trust.
///
/// Free of `ShareClient` because the swarm asks several peers at once, and only
/// one of them is the connection this tab is homed on.
async fn fetch_chunk_on(
    conn: &Connection,
    token: &[u8; SECRET_LEN],
    address: ChunkHash,
) -> Result<Option<Vec<u8>>, JsValue> {
    let (mut send, mut recv) = conn
        .open_bi()
        .await
        .map_err(|error| err("open chunk stream", &error))?;
    send.write_all(&framing::encode_chunk_request(token, address.as_bytes()))
        .await
        .map_err(|error| err("send chunk request", &error))?;
    send.finish().map_err(|error| err("finish", &error))?;

    let mut status = [0u8; 1];
    if recv.read_exact(&mut status).await.is_err() {
        return Ok(None);
    }
    if status[0] != agent_share_proto::manifest::ReadStatus::Ok.to_byte() {
        return Ok(None);
    }
    let len = read_len(&mut recv, framing::MAX_CHUNK_LEN).await?;
    let mut bytes = vec![0u8; len as usize];
    recv.read_exact(&mut bytes)
        .await
        .map_err(|error| err("read chunk", &error))?;
    Ok(Some(bytes))
}

/// Which chunks of `root` a peer says it can serve.
///
/// **The first consumer `OP_HAVE` has ever had.** It was implemented and served
/// from the start of the chunk work and called by nobody, because with one
/// connection there was nothing to schedule across; asking a single peer what
/// it holds only to ask it for the same bytes anyway buys a round trip and no
/// information.
///
/// `None` for a peer that will not answer — no chunk table, a root it never
/// heard of, or a version of the protocol without the op. All three mean "plan
/// without this peer", which costs a fallback and never a wrong chunk.
async fn fetch_have_on(
    conn: &Connection,
    token: &[u8; SECRET_LEN],
    root: Root,
    chunks: usize,
) -> Option<Coverage> {
    let (mut send, mut recv) = conn.open_bi().await.ok()?;
    send.write_all(&framing::encode_have_request(token, root.as_bytes()))
        .await
        .ok()?;
    send.finish().ok()?;

    let mut status = [0u8; 1];
    recv.read_exact(&mut status).await.ok()?;
    if status[0] != agent_share_proto::manifest::ReadStatus::Ok.to_byte() {
        return None;
    }
    let len = read_len(&mut recv, framing::MAX_CHUNK_MAP_BYTES).await.ok()?;
    let mut body = vec![0u8; len as usize];
    recv.read_exact(&mut body).await.ok()?;
    let (claimed, bitmap) = framing::decode_have(&body).ok()?;
    // Held to the length *we* know the row to be, not the one the answer
    // claims. A peer that overstates its row would otherwise have chunks
    // scheduled at positions the file does not have.
    if claimed as usize != chunks {
        return None;
    }
    Coverage::from_bits(&bitmap, chunks).ok()
}

/// One manifest round on `conn`: the exact bytes served, and their decoding.
///
/// Free of `ShareClient` so the seeder fallback can vet a candidate *before*
/// any client exists; the method wraps this and adds the card publish.
async fn fetch_manifest_on(
    conn: &Connection,
    token: &[u8; SECRET_LEN],
    author: Option<[u8; 32]>,
) -> Result<FetchedManifest, JsValue> {
    let (mut send, mut recv) = conn
        .open_bi()
        .await
        .map_err(|error| stream_open_failed("could not fetch the listing", &error))?;
    send.write_all(&framing::encode_manifest_request(token))
        .await
        .map_err(|error| err("send manifest request", &error))?;
    send.finish().map_err(|error| err("finish", &error))?;

    let len = read_header(&mut recv, framing::MAX_SIGNED_MANIFEST_BYTES).await?;
    let mut envelope = vec![0u8; len as usize];
    recv.read_exact(&mut envelope)
        .await
        .map_err(|error| err("read manifest", &error))?;
    let signed =
        SignedManifest::decode(&envelope).map_err(|error| err("decode manifest", &error))?;
    if u32::try_from(signed.manifest.len()).is_ok_and(|len| len > MAX_MANIFEST_BYTES) {
        return Err(JsValue::from_str("the manifest is too large to accept"));
    }
    accept_manifest(author, &signed, accepted_version(token))?;
    let manifest =
        MountManifest::decode(&signed.manifest).map_err(|error| err("decode manifest", &error))?;
    Ok(FetchedManifest {
        envelope,
        body: signed.manifest,
        manifest,
    })
}

/// Check a manifest against the share's creator, when the ticket names one.
///
/// The browser's half of the rule the native consumer keeps: **whether to
/// verify is read off the ticket**, which the user pasted, never off the
/// answer, which any seeder could have written. A ticket with no author is an
/// unsigned share and is accepted as it always was.
fn accept_manifest(
    author: Option<[u8; 32]>,
    signed: &SignedManifest,
    seen: u64,
) -> Result<(), JsValue> {
    let Some(author) = author else {
        return Ok(());
    };
    let author = PublicKey::from_bytes(&author)
        .map_err(|_| JsValue::from_str("the ticket's authorship key is not a public key"))?;
    signed
        .accept(&author, seen)
        .map_err(|error| JsValue::from_str(&format!("{error:#}")))
}

/// The highest manifest version this tab has already accepted for `token`.
///
/// Recorded beside the persisted manifest, which is what makes the rollback
/// rule mean something here and not on the native side: a browser keeps state
/// across reloads, so an old-but-genuinely-signed manifest replayed by a seeder
/// has something to lose to. `0` for a tab that has never stored one, which
/// accepts anything the signature allows.
fn accepted_version(token: &[u8; SECRET_LEN]) -> u64 {
    let Some(storage) = local_storage() else {
        return 0;
    };
    let Ok(Some(raw)) = storage.get_item(&manifest_locator_key(token)) else {
        return 0;
    };
    serde_json::from_str::<serde_json::Value>(&raw)
        .ok()
        .and_then(|locator| locator.get("version")?.as_u64())
        .unwrap_or(0)
}

/// How long the probe tolerates **zero forward progress** before declaring
/// the path unable to carry bulk.
///
/// A window, not a total budget: the clock re-arms on every chunk, so a slow
/// link passes as long as bytes keep arriving. The defect this probe hunts
/// is a total freeze — a read frozen at its first bytes forever — and a
/// freeze does not trickle. A whole-read deadline here would be a throughput
/// floor (256 KiB in 10 s demands ≥26 KB/s) that discards healthy seeders on
/// slow links, and a slow seeder holding the only copy beats no seeder.
const PROBE_STALL_MS: i32 = 10_000;

/// Bytes per probe body read. Sub-frame granularity is what makes the stall
/// window a progress meter rather than a second whole-read deadline: a slow
/// link renews the window on every chunk it manages to land.
const PROBE_CHUNK_LEN: usize = 16 * 1024;

/// Slack for the parts of vetting a candidate that carry no deadline of
/// their own: the relay dial after the data channel concedes, then fetching
/// and hashing the manifest. Both are ordinary round trips against a peer
/// that has already answered something.
const VET_OVERHEAD_MS: i32 = 10_000;

/// How many [`PROBE_STALL_MS`] windows a lane allows a bulk probe.
///
/// One to set up and land first bytes, one for a slow drain to re-arm into.
/// The probe re-arms per chunk with no total bound by design, so no finite
/// deadline can promise every slow-but-moving seeder completes; what this
/// buys is that a probe is not cut before it has had a fair chance.
const PROBE_WINDOWS_ALLOWED: i32 = 2;

/// The least a lane may allow one candidate and still finish it: a full
/// channel wait, then the relay fallback and manifest fetch behind it.
///
/// A lane deadline under this can never complete a single candidate — it
/// expires while the first one is still deciding whether to concede the
/// relay — so the lane can only ever report failure, however healthy the
/// peers are.
const fn vet_floor_ms(channel_wait_ms: i32) -> i32 {
    channel_wait_ms + VET_OVERHEAD_MS
}

/// What one candidate needs end to end, bulk probe included.
const fn vet_budget_ms(channel_wait_ms: i32) -> i32 {
    vet_floor_ms(channel_wait_ms) + PROBE_STALL_MS * PROBE_WINDOWS_ALLOWED
}

/// The tick a background tab's timers are clamped to.
///
/// Measured in this project against Safari at 13-20 s; the upper end is the
/// one to size against, since being wrong the other way is what this guards.
#[cfg(test)]
const BACKGROUND_TICK_MS: f64 = 20_000.0;

/// How many times a polled wait must actually run before its clock may
/// expire it.
const MIN_POLLS_BEFORE_GIVING_UP: u32 = 3;

/// Whether a polled wait is out of time.
///
/// Wall clock alone is wrong for anything a hidden tab runs, and reconnect
/// is exactly that: the browser throttles timers past the point where the
/// connection survives, so hiding the tab is what kills it in the first
/// place. A `wait_ms(500)` inside a hidden tab returns 13-20 s later, so a
/// budget shorter than one tick gives that tab a single attempt at whatever
/// it is waiting for — the first poll is also the last. Requiring a minimum
/// number of polls keeps the budget honest in both worlds: a foreground tab
/// blows through the poll count long before the clock, and a throttled one
/// gets its chances however long the browser takes to hand them over.
fn out_of_time(elapsed_ms: f64, polls: u32, budget_ms: f64) -> bool {
    elapsed_ms > budget_ms && polls >= MIN_POLLS_BEFORE_GIVING_UP
}

/// One ranged read on `conn`, failed only if it stops moving.
///
/// The connect-time bulk probe: between two browser tabs the data channel
/// passes JSEP and a manifest fine and then stalls on bulk READ responses —
/// observed as a zip download frozen at its 39-byte local header on an
/// otherwise healthy `paths webrtc` mount, twice, on a pristine mesh. Until
/// the transport's browser↔browser bulk path is fixed, a data-channel
/// connection must prove it can move a real chunk before it is allowed to
/// carry the share; a channel that cannot is closed and the relay takes the
/// job. Only that pairing is suspect: the caller does not probe relay or ip
/// connections, where bulk never stalled.
async fn probe_read(
    conn: &Connection,
    token: &[u8; SECRET_LEN],
    index: u32,
    len: u32,
) -> Result<(), JsValue> {
    // Open + request + header under one stall window: a frozen channel does
    // not answer the header either, and nothing here is throughput-bound.
    let setup = async {
        let (mut send, mut recv) = conn
            .open_bi()
            .await
            .map_err(|error| err("open probe stream", &error))?;
        send.write_all(&framing::encode_read_request(token, index, 0, len))
            .await
            .map_err(|error| err("send probe read", &error))?;
        send.finish().map_err(|error| err("finish", &error))?;
        let got = read_header(&mut recv, len).await?;
        Ok::<_, JsValue>((recv, got))
    };
    let (recv, got) =
        match futures::future::select(Box::pin(setup), Box::pin(wait_ms(PROBE_STALL_MS))).await {
            futures::future::Either::Left((outcome, _)) => outcome?,
            futures::future::Either::Right(((), _)) => {
                return Err(JsValue::from_str(
                    "probe read stalled: this path cannot carry bulk data",
                ));
            }
        };
    // The full request or nothing: callers size `len` to what the file can
    // serve (`probe_target`), so a shorter answer proves less than asked —
    // under the old any-bytes rule a 1 KiB file "passed" a 256 KiB probe.
    if got < len {
        return Err(JsValue::from_str(&format!(
            "probe read answered {got} of {len} bytes: too short to prove bulk"
        )));
    }
    let mut source = StreamChunks {
        recv,
        buf: vec![0u8; PROBE_CHUNK_LEN],
    };
    drain_with_stall_deadline(&mut source, got as usize, || wait_ms(PROBE_STALL_MS)).await
}

/// Which file the bulk probe reads, and how much: the **largest** live file,
/// asking for everything it can serve up to one `MAX_READ_LEN` window.
///
/// Largest, not first. The seeder clamps a read to the file's size, so a
/// small file ahead of the payload would shrink the probe to nothing — a
/// share fronted by a 1 KiB README once green-lit the channel with a 1 KiB
/// read and then froze on the first real 256 KiB frame, exactly the stall
/// the probe exists to catch. The largest file is what the transfer will
/// actually do to the channel; if even that is small, the share's real reads
/// are small too and the probe stays honest. `None` when every entry is a
/// tombstone or empty — nothing bulk will ever be read.
fn probe_target(files: &[agent_share_proto::manifest::FileEntry]) -> Option<(u32, u32)> {
    files
        .iter()
        .enumerate()
        .filter(|(_, entry)| !entry.is_tombstone() && entry.size > 0)
        .max_by_key(|(_, entry)| entry.size)
        .map(|(index, entry)| {
            let want =
                u32::try_from(entry.size.min(u64::from(MAX_READ_LEN))).unwrap_or(MAX_READ_LEN);
            (u32::try_from(index).unwrap_or(0), want)
        })
}

/// The probe's chunk supply — a seam so the rolling-deadline drain can run
/// against a scripted stream in tests, where no [`Connection`] exists.
trait ProbeChunkSource {
    /// Read some bytes; `Ok(None)` means the stream ended.
    async fn next_chunk(&mut self) -> Result<Option<usize>, JsValue>;
}

struct StreamChunks {
    recv: fofoca::iroh::endpoint::RecvStream,
    buf: Vec<u8>,
}

impl ProbeChunkSource for StreamChunks {
    async fn next_chunk(&mut self) -> Result<Option<usize>, JsValue> {
        self.recv
            .read(&mut self.buf)
            .await
            .map_err(|error| err("read probe body", &error))
    }
}

/// Pull `remaining` bytes out of `source`, racing **each** read against a
/// fresh `stall_timer`. Progress re-arms the clock; only a full window with
/// no bytes at all fails — see [`PROBE_STALL_MS`] for why slow must pass.
/// The timer is a parameter because the production clock (`wait_ms`) needs a
/// `Window`, which the wasm test runner does not have.
async fn drain_with_stall_deadline<Source, Timer, TimerFut>(
    source: &mut Source,
    mut remaining: usize,
    mut stall_timer: Timer,
) -> Result<(), JsValue>
where
    Source: ProbeChunkSource,
    Timer: FnMut() -> TimerFut,
    TimerFut: std::future::Future<Output = ()>,
{
    while remaining > 0 {
        match futures::future::select(Box::pin(source.next_chunk()), Box::pin(stall_timer())).await
        {
            futures::future::Either::Left((Ok(Some(read)), _)) if read > 0 => {
                remaining = remaining.saturating_sub(read);
            }
            futures::future::Either::Left((Ok(_), _)) => {
                return Err(JsValue::from_str("probe read body ended early"));
            }
            futures::future::Either::Left((Err(error), _)) => return Err(error),
            futures::future::Either::Right(((), _)) => {
                return Err(JsValue::from_str(
                    "probe read stalled: this path cannot carry bulk data",
                ));
            }
        }
    }
    Ok(())
}

/// How long **one attempt** waits for peer cards to arrive over gossip.
///
/// Per-attempt, not terminal: the caller retries for as long as the page
/// lives, and the membership below persists across attempts — so this bound
/// only decides how often control returns to the caller (which wants to retry
/// the *origin* too). Cards keep accumulating on the persistent membership
/// while the App backs off, so a short attempt loses nothing; it just hands
/// the origin its turn sooner. Sized past fofoca's fast recovery lanes (the
/// 6 s beacon-reclaim window, the 10 s empty-mesh claim grace) while leaving
/// the slow island-merge cadence (~30–60 s) to the *next* attempt.
const SEEDER_CARDS_DEADLINE_MS: f64 = 12_000.0;

/// Where a share's blocks live.
///
/// Keyed by the mesh id, which is a one-way hash of the token — so two
/// shares never share a database, and the token itself never reaches a
/// name that storage inspectors or `about:` pages would display. A free
/// function because the dead-origin re-arm needs it before any client
/// exists.
fn store_name_for(token: &[u8; SECRET_LEN]) -> String {
    format!("agent-share/{}", &share_mesh_key(token)[..16])
}


/// Where the manifest *locator* lives: `localStorage`, beside the store.
///
/// The bytes are in the [`IdbStore`]; this tiny `{ size, tree }` record is
/// what makes them findable on the next load — reconstructing the store's
/// `FileId` needs the size, and the fingerprint is the integrity check.
fn manifest_locator_key(token: &[u8; SECRET_LEN]) -> String {
    format!("agent-share/manifest/{}", &share_mesh_key(token)[..16])
}

fn local_storage() -> Option<web_sys::Storage> {
    web_sys::window().and_then(|window| window.local_storage().ok().flatten())
}

/// A seeder this tab once **vetted** — manifest hashed against its claim,
/// data channel bulk-probed — recorded so the next reconnect can redial it
/// directly instead of waiting for the mesh to reintroduce everyone. Never
/// written from raw cards, which may be ghosts.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct KnownSeeder {
    endpoint: String,
    /// The manifest fingerprint the endpoint served when it won; the redial
    /// re-verifies against this, so a stale record is refused, not trusted.
    tree: String,
    seen_ms: f64,
    /// Consecutive attempts that could not reach this endpoint at all. See
    /// [`KNOWN_SEEDER_STRIKE_LIMIT`]. Defaulted so rosters written before
    /// this field existed still decode.
    #[serde(default)]
    strikes: u32,
}

/// Where the vetted-seeder records live: `localStorage`, beside the manifest
/// locator, under the same hashed-name privacy rationale. The values are
/// endpoint ids — public keys — so nothing secret lands in a name or value a
/// storage inspector would display.
fn known_seeders_key(token: &[u8; SECRET_LEN]) -> String {
    format!("agent-share/seeders/{}", &share_mesh_key(token)[..16])
}

/// Roster ceiling: enough for every live peer of a small share, small enough
/// that a redial lane full of corpses stays cheap.
const KNOWN_SEEDERS_CAP: usize = 4;

/// Records older than this are corpses with near certainty — a browser
/// endpoint identity does not survive a reload, let alone a day.
const KNOWN_SEEDER_TTL_MS: f64 = 24.0 * 60.0 * 60.0 * 1_000.0;

/// Decode a stored roster, dropping expired entries and anything past the
/// cap. Tolerant of garbage: an unparseable record reads as empty.
fn decode_known_seeders(raw: &str, now: f64) -> Vec<KnownSeeder> {
    let mut list: Vec<KnownSeeder> = serde_json::from_str(raw).unwrap_or_default();
    list.retain(|entry| now - entry.seen_ms < KNOWN_SEEDER_TTL_MS);
    list.truncate(KNOWN_SEEDERS_CAP);
    list
}

/// Newest first, one record per endpoint, capped.
fn upsert_known_seeder(mut list: Vec<KnownSeeder>, entry: KnownSeeder) -> Vec<KnownSeeder> {
    list.retain(|known| known.endpoint != entry.endpoint);
    list.insert(0, entry);
    list.truncate(KNOWN_SEEDERS_CAP);
    list
}

fn load_known_seeders(token: &[u8; SECRET_LEN]) -> Vec<KnownSeeder> {
    local_storage()
        .and_then(|storage| storage.get_item(&known_seeders_key(token)).ok().flatten())
        .map(|raw| decode_known_seeders(&raw, now_ms()))
        .unwrap_or_default()
}

/// Best-effort, like every `localStorage` write here: a refusal costs the
/// fast lane on the next reconnect, never the session.
fn store_known_seeders(token: &[u8; SECRET_LEN], list: &[KnownSeeder]) {
    if let (Some(storage), Ok(raw)) = (local_storage(), serde_json::to_string(list)) {
        let _ = storage.set_item(&known_seeders_key(token), &raw);
    }
}

/// Record a seeder the mesh vouched for. The [`MajorityTree`] is the whole
/// admission ticket: a caller that adopted on its own memory rather than on
/// a vote has none to offer, and so cannot refresh the record's clock.
fn remember_known_seeder(token: &[u8; SECRET_LEN], endpoint: &str, tree: &MajorityTree) {
    let list = upsert_known_seeder(
        load_known_seeders(token),
        KnownSeeder {
            endpoint: endpoint.to_owned(),
            tree: tree.as_str().to_owned(),
            seen_ms: now_ms(),
            strikes: 0,
        },
    );
    store_known_seeders(token, &list);
}

/// A tree the mesh actually voted for, as opposed to one this tab merely
/// remembers. Only [`majority_tree`] mints it, and [`remember_known_seeder`]
/// demands it — so a lane that skipped the vote cannot write to the roster,
/// which is what stops a record from renewing its own 24 h life every time
/// it wins and outliving the tree it names.
#[derive(Clone, Debug, PartialEq, Eq)]
struct MajorityTree(String);

impl MajorityTree {
    fn as_str(&self) -> &str {
        &self.0
    }
}

/// The tree the most cards vouch for, or `None` when nobody vouches.
///
/// With the origin gone, agreement is the only manifest authority left.
/// Ghost cards from departed peers vote too — a known defect of the roster —
/// but a ghost that voted *for* the majority costs nothing, and one that
/// formed a majority alone still cannot answer a dial. That tolerance is why
/// callers may use this to *choose*, and why [`judge_adopted_tree`] only
/// lets it demote a record rather than tear down a working connection.
fn majority_tree(cards: &[agent_share_proto::PeerCard]) -> Option<MajorityTree> {
    let mut votes: HashMap<&str, usize> = HashMap::new();
    for card in cards {
        if let Some(tree) = card.tree.as_deref() {
            *votes.entry(tree).or_default() += 1;
        }
    }
    votes
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(tree, _)| MajorityTree(tree.to_owned()))
}

/// What the roster thinks of a tree this tab already adopted.
#[derive(Debug, PartialEq, Eq)]
enum TreeVerdict {
    /// Nobody vouches yet: the roster has nothing to say.
    Unknown,
    Agrees,
    /// The mesh has moved on, and this tab's memory of who serves what is
    /// what led it here.
    Disagrees,
}

/// Judge the tree a known-seeder redial pinned against the roster that
/// arrived afterwards.
///
/// The redial lane trades the majority vote for speed: it dials the peers
/// this tab remembers and pins whichever tree they serve, so a seeder frozen
/// on an outdated tree is adopted with nobody to contradict it. This is the
/// contradiction, arriving late. `own` is this tab's own mesh id, whose card
/// vouches for the tree it just adopted and would otherwise vote for itself.
fn judge_adopted_tree(
    cards: &[agent_share_proto::PeerCard],
    own: &str,
    pinned: &str,
) -> TreeVerdict {
    let others: Vec<agent_share_proto::PeerCard> = cards
        .iter()
        .filter(|card| card.endpoint != own && card.tree.is_some() && card.serving.is_some())
        .cloned()
        .collect();
    match majority_tree(&others) {
        None => TreeVerdict::Unknown,
        Some(majority) if majority.as_str() == pinned => TreeVerdict::Agrees,
        Some(_) => TreeVerdict::Disagrees,
    }
}

/// Drop named endpoints from the roster outright. For a peer the mesh has
/// contradicted, where the record itself is what is wrong — see
/// [`challenge_adopted_tree`].
fn forget_known_seeders(token: &[u8; SECRET_LEN], failed: &[String]) {
    let mut list = load_known_seeders(token);
    list.retain(|entry| !failed.contains(&entry.endpoint));
    store_known_seeders(token, &list);
}

/// The attempt log, joined for a user-facing error.
fn refusal_lines(refusals: &[Refusal]) -> String {
    refusals
        .iter()
        .map(|refusal| refusal.reason.as_str())
        .collect::<Vec<_>>()
        .join("; ")
}

/// How many attempts in a row must fail to *reach* a seeder before its
/// record is dropped.
///
/// Not one. A dial that reaches nobody is ambiguous: the peer may be gone,
/// or this tab's own link may be down — a laptop waking with its interfaces
/// still cold fails every dial in milliseconds, and the roster is what the
/// fast lane needs most at exactly that moment. One miss is not evidence.
/// Three in a row is, and any answer in between clears the count.
const KNOWN_SEEDER_STRIKE_LIMIT: u32 = 3;

/// The roster this tab keeps after a redial lane that produced no winner.
///
/// The rule is that a record is only spent by evidence about *the peer*.
/// The lane used to delete every endpoint it had launched an attempt
/// against, which threw away three different kinds of live seeder: one that
/// answered and then failed its vet, one still mid-dial when the lane's
/// deadline fired, and — when the local link was down — all of them at once.
///
/// So an endpoint that never refused is left alone, since an attempt cut
/// off mid-flight says nothing; an endpoint that answered has its count
/// cleared, since whatever failed afterwards was about the share and not
/// about the peer being there; and an endpoint nothing could reach is
/// struck, and dropped only once it has run out of chances.
fn roster_after_failed_lane(list: Vec<KnownSeeder>, refusals: &[Refusal]) -> Vec<KnownSeeder> {
    list.into_iter()
        .filter_map(|mut entry| {
            let Some(refusal) = refusals
                .iter()
                .find(|refusal| refusal.endpoint == entry.endpoint)
            else {
                return Some(entry);
            };
            if !refusal.unreached {
                entry.strikes = 0;
                return Some(entry);
            }
            entry.strikes += 1;
            (entry.strikes < KNOWN_SEEDER_STRIKE_LIMIT).then_some(entry)
        })
        .collect()
}

/// Persist the origin's manifest so a refreshed tab can re-arm with no live
/// source — the web twin of the native mirror's sidecar. Best-effort: a full
/// quota or private-mode refusal costs resurrection, never the session.
/// `envelope` is the whole `OP_MANIFEST` body, signature included, because that
/// is what this tab will re-serve after a reload. The locator beside it records
/// the *manifest's* fingerprint and version — over the bytes inside, since that
/// is the domain `card.tree` and the rollback rule are defined on.
async fn persist_manifest(token: &[u8; SECRET_LEN], store: &IdbStore, envelope: &[u8]) {
    let Ok(signed) = SignedManifest::decode(envelope) else {
        return;
    };
    let bytes = envelope;
    // The manifest is stored as a file like any other: a row of addresses plus
    // its chunks. No special case, no second storage shape, and the row's root
    // is the locator — so a manifest that was written half-way simply fails to
    // reassemble rather than resurrecting a tree nobody ever published.
    let row = ChunkMap::build(bytes);
    for position in 0..row.len() {
        let range = row.range_of(position);
        let (Some(address), Some(slice)) = (
            row.leaf(position),
            bytes.get(range.start as usize..range.end as usize),
        ) else {
            return;
        };
        if let Err(error) = store.put(address, slice).await {
            web_sys::console::debug_1(&JsValue::from_str(&format!(
                "[share] persisting the manifest failed: {error}"
            )));
            return;
        }
    }
    if let Err(error) = store.put_map(&row).await {
        web_sys::console::debug_1(&JsValue::from_str(&format!(
            "[share] persisting the manifest row failed: {error}"
        )));
        return;
    }
    let locator = serde_json::json!({
        "size": bytes.len(),
        "root": row.root().to_hex(),
        "tree": agent_share_proto::manifest::manifest_fingerprint(&signed.manifest),
        "version": signed.version,
    });
    if let Some(storage) = local_storage() {
        let _ = storage.set_item(&manifest_locator_key(token), &locator.to_string());
    }
}

/// The persisted manifest, verified against its recorded fingerprint, plus
/// the store it came from. `None` for a tab that never seeded here, a
/// cleared storage, or a record that fails its own integrity check — all of
/// which re-arm nothing and fall back to waiting for a live peer.
async fn load_persisted_manifest(
    token: &[u8; SECRET_LEN],
) -> Option<(Vec<u8>, String, MountManifest, Arc<IdbStore>)> {
    let storage = local_storage()?;
    let raw = storage.get_item(&manifest_locator_key(token)).ok()??;
    let locator: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let tree = locator.get("tree")?.as_str()?;
    let root = Root::from_hex(locator.get("root")?.as_str()?).ok()?;
    // Adopt-only: a tab reaching this path has a locator, which only a sync in
    // this origin could have written — so the database exists, and creating one
    // here would leave storage behind for somebody who merely browsed.
    let store = Arc::new(IdbStore::adopt(&store_name_for(token)).await.ok()??);
    let row = store.map(root).await.ok()??;
    let mut envelope = Vec::with_capacity(row.size() as usize);
    for position in 0..row.len() {
        let address = row.leaf(position)?;
        let chunk = store.get(address).await.ok()??;
        envelope.extend_from_slice(&chunk);
    }
    let signed = SignedManifest::decode(&envelope).ok()?;
    // The recorded fingerprint, over the manifest and not the envelope: it has
    // to be the same string a peer card carries, or a resurrected tab would
    // vouch for a tree nobody else recognises.
    if agent_share_proto::manifest::manifest_fingerprint(&signed.manifest) != tree {
        return None;
    }
    let tree = tree.to_owned();
    let manifest = MountManifest::decode(&signed.manifest).ok()?;
    Some((envelope, tree, manifest, store))
}

/// Take back what this tab can no longer serve.
///
/// The browser evicts IndexedDB under quota pressure without asking, and the
/// card is the one claim that cannot notice. Serving itself is already honest —
/// a hole answers `BadIndex`, and `OP_HAVE` re-reads the store every time — but
/// until the `serving` set is corrected, readers keep being routed here and keep
/// being refused. Hypercore names the same obligation on `core.clear()`: a peer
/// whose holdings shrank gossips that they did.
///
/// The seeder reports the slot and passes no verdict, because a refusal is not
/// proof of loss: reading into a hole of a slot this tab never advertised is
/// ordinary. So the claim is the filter, and the store is the authority — see
/// [`agent_share_mount::Seeder::holes`], which also records why only the read
/// path reports and what that costs.
///
/// Holds a weak seeder, so a client the app dropped does not keep this tab's
/// rows and chunk-address set alive behind a task nobody will poll again.
async fn watch_for_lost_holdings(
    seeder: agent_share_mount::WeakSeeder<IdbStore>,
    mesh: Rc<RefCell<MeshSlot>>,
    card: Rc<RefCell<CardHoldings>>,
) {
    use futures::StreamExt as _;

    // Subscribed through a strong handle, which is then dropped: from here the
    // task holds nothing that would stop the seeder being collected, and the
    // feed ends by itself when it is.
    let mut holes = {
        let Some(seeder) = seeder.upgrade() else {
            return;
        };
        seeder.holes()
    };
    while let Some(index) = holes.next().await {
        let Some(seeder) = seeder.upgrade() else {
            return;
        };
        if !card.borrow().held.contains(&index) {
            // Never advertised, so nothing to take back: reading into a hole of
            // a partially-held slot is ordinary.
            continue;
        }
        // Only the slot that was reported. Re-deriving the whole share would
        // cost one coverage lookup per file, and `IdbStore::coverage`
        // enumerates every stored address on each of them.
        if seeder.holds(index).await {
            // Refused, yet the store still backs it: a read that raced an
            // update, not an eviction. Nothing to take back.
            continue;
        }
        let published = {
            let mut card = card.borrow_mut();
            card.held.remove(&index);
            card.serving()
        };
        web_sys::console::warn_1(&JsValue::from_str(&format!(
            "[share] slot {index} is no longer in storage (evicted?); \
             retracting it from the card"
        )));
        // Cloned out of the cell, never borrowed across the await: see the note
        // on the `mesh` field.
        let Some(peer) = mesh_peer_of(&mesh) else {
            continue;
        };
        peer.set_serving(published).await;
    }
}

/// What this tab advertises it can serve, and what it takes to encode that.
///
/// The two travel together because neither means anything alone: a set of
/// indices needs the slot count they index into, and publishing one against a
/// stale other mis-states the card. Shared so the availability grid, the publish
/// path and [`watch_for_lost_holdings`] all read one copy.
#[derive(Debug, Default)]
struct CardHoldings {
    /// Manifest indices held in full.
    ///
    /// Indices rather than paths because that is what a `READ` addresses and
    /// what the availability grid paints. Recomputed from the store rather than
    /// accumulated, so a reload shows what actually survived instead of what
    /// this session happened to fetch.
    held: BTreeSet<u32>,
    /// Live slots in the manifest these index into.
    slots: usize,
}

impl CardHoldings {
    /// The `serving` field for a peer card.
    fn serving(&self) -> Option<String> {
        let held: Vec<u32> = self.held.iter().copied().collect();
        agent_share_proto::serving::encode_serving(&held, self.slots)
    }
}

/// The mesh peer, when joined. `Pending`/`Left` read as "no mesh".
///
/// Takes the cell rather than `&self` so the retraction task, which outlives no
/// `ShareClient`, resolves the peer the same way [`ShareClient::mesh_peer`] does.
fn mesh_peer_of(mesh: &RefCell<MeshSlot>) -> Option<Rc<mesh::MeshPeer>> {
    match &*mesh.borrow() {
        MeshSlot::Joined(peer) => Some(Rc::clone(peer)),
        MeshSlot::Pending | MeshSlot::Left => None,
    }
}

/// Chunk rows this store already holds for `manifest`, by manifest index.
///
/// The live set, derived from **persisted rows** rather than from anything this
/// session happened to fetch: a tally that lost an entry to a reload would make
/// this tab claim less than it has, and would make a later sweep delete more
/// than it should.
///
/// A row is found by re-deriving the file's root from its own bytes, which the
/// store cannot do — so the mapping from slot to root is recovered by asking
/// the store for a row bound to that file version.
async fn rows_in_store(store: &IdbStore, manifest: &MountManifest) -> HashMap<u32, ChunkMap> {
    let mut rows = HashMap::new();
    for (index, entry) in manifest.files.iter().enumerate() {
        if entry.is_tombstone() {
            continue;
        }
        let Ok(index) = u32::try_from(index) else {
            continue;
        };
        // The bind table is what survives a reload: it maps this file version
        // to the root whose row describes it.
        if let Ok(Some(root)) = store.bind(&file_id(entry)).await
            && let Ok(Some(row)) = store.map(root).await
        {
            rows.insert(index, row);
        }
    }
    rows
}

/// A share-mesh membership waiting for peers, alive across connect attempts.
///
/// Carries the full two-endpoint WebRTC shape so a producer-less share moves
/// bytes peer-to-peer, not over the relay:
///
/// - `endpoint` is the relay-bearing half — it rides the mesh, serves the
///   share's ALPNs on the mesh Router (`produce.rs`'s server shape: inbound
///   JSEP answers attach into its hub, inbound mount dials arrive on its
///   custom transport), and carries this tab's *outbound* JSEP offers.
/// - `mount_endpoint` is the relay-free twin on the same key: the endpoint
///   whose only possible path to a seeder is the data channel, which is what
///   makes the mount *settle* on WebRTC instead of losing the race to a warm
///   relay path — the same split `connect_webrtc` measured its way into.
#[derive(Clone)]
struct WaitingMesh {
    peer: Rc<mesh::MeshPeer>,
    seeder: seed::SeederShared,
    endpoint: Endpoint,
    /// The mount identity's relay-bearing half, for JSEP only. It exists
    /// because a seeder's refusal is keyed to the **TLS-proven id of the
    /// signal connection** — an offer sent over the mesh endpoint is refused
    /// for the mesh lane's session no matter whose id the envelope claims.
    signal_endpoint: Endpoint,
    mount_endpoint: Endpoint,
    /// The mount lane's session registry: outbound seeder sessions attach
    /// here; `has_session` is what turns a retry into a free dial.
    mount_hub: Arc<BrowserHubTransport>,
    /// The mesh lane's session registry, held for one reason: so that
    /// [`Self::retire`] can let go of it. A hub's sessions close through
    /// `detach`, never through `Endpoint::close()` — which is why
    /// `produce.rs`'s `stop()` detaches before closing — so a membership
    /// retired without this left the mesh lane's peer connections open with
    /// their handlers installed, and remote tabs kept live-looking channels
    /// to an identity that had departed.
    mesh_hub: Arc<BrowserHubTransport>,
    /// Set when a connect of this tab's has joined the share's mesh under
    /// *another* identity, which makes this one a duplicate: two cards and
    /// two relay registrations for one tab on one share. It cannot always be
    /// collected the moment that is discovered, because a client may still
    /// be homed on it, so the finding is recorded here and acted on by
    /// whoever lets go last — see [`collect_duplicate_now`].
    duplicate: Rc<Cell<bool>>,
}

impl WaitingMesh {
    /// Graceful farewell for a membership nothing will reuse. `leave` says
    /// whether the mesh goodbye is still owed — a caller that already left
    /// through this same `peer` must not broadcast twice.
    ///
    /// The closes are the point. iroh's `Drop` aborts without the closing
    /// handshake, so a dropped membership leaves its relay registrations
    /// (two of them — the mesh identity and the signal half) and its peers'
    /// connections to time out on their own. Those corpses are what make
    /// rendezvous vacancy probes read "held" and pad rosters with ghosts
    /// that the candidate race then pays to rule out. The origin path has
    /// always closed its endpoints on farewell; this is the waiting lane
    /// catching up. Endpoints first — the close frames ride any live data
    /// channels — then the hub lets go of the peer connections themselves.
    async fn retire(self, leave: bool) {
        if leave {
            let _ = self.peer.leave().await;
        }
        self.endpoint.close().await;
        self.signal_endpoint.close().await;
        self.mount_endpoint.close().await;
        self.detach_hubs();
    }

    /// Let go of every peer connection this membership holds.
    ///
    /// Both hubs, and through one loop rather than one line each, so that a
    /// third lane cannot be added and quietly left attached — the mesh hub
    /// was missing here for exactly that reason. Synchronous, which is what
    /// makes it the only part of a farewell a page teardown can still run;
    /// see [`ShareClient::shutdown_mesh`].
    fn detach_hubs(&self) {
        for hub in [&self.mount_hub, &self.mesh_hub] {
            hub.detach_all();
        }
    }
}

/// Put `mine` in the registry under `key`, or hand back whoever got there
/// first.
///
/// Minting a membership takes three endpoint binds and a mesh join, all of
/// them `await` points, so two attempts for one share routinely overlap —
/// a `/files` → `/info` → back navigation remounts the Session and starts a
/// second one while the first is still binding. Both then find the registry
/// empty. A plain `insert` silently drops whichever landed first, and a
/// dropped `WaitingMesh` is not a free one: it is a joined membership with a
/// heartbeating card and two relay registrations that nothing will ever say
/// goodbye for. `Some` means the caller lost and owes its own entry a
/// `retire`; `None` means the registry now holds it.
fn claim_waiting_mesh<T: Clone>(
    registry: &RefCell<HashMap<String, T>>,
    key: &str,
    mine: T,
) -> Option<T> {
    let mut registry = registry.borrow_mut();
    match registry.get(key) {
        Some(winner) => Some(winner.clone()),
        None => {
            registry.insert(key.to_owned(), mine);
            None
        }
    }
}

/// References to a waiting membership's peer when nothing but the registry
/// holds it. [`adopt_vetted`] hands each adopting client another (its
/// `MeshSlot::Joined`), and an attempt in flight holds one for as long as it
/// runs, so anything above this baseline means somebody is still living on
/// these endpoints.
const REGISTRY_ONLY_PEER_REFS: usize = 1;

/// Whether a waiting membership may be closed out from under whoever else
/// might hold it.
///
/// `Endpoint` clones share one `Arc`'d socket, so `close()` on any clone
/// takes down every holder at once — a seeder-lane client adopted from this
/// entry serves its peers over the very same endpoints. Counting references
/// rather than tracking a flag means a client releasing itself restores the
/// membership's retirability for free, and it fails in the safe direction:
/// an unexpected clone postpones a cleanup instead of severing a live peer.
fn safe_to_retire(peer_refs: usize) -> bool {
    peer_refs <= REGISTRY_ONLY_PEER_REFS
}

/// Whether a membership already found to be a duplicate may be collected.
///
/// The mark is what keeps this narrow. A membership nobody is using is not
/// by itself garbage: a tab that navigated away from a share keeps serving
/// it from exactly such an entry, which is how a swarm keeps its copies
/// after everyone has closed the tab. Only a membership this tab has since
/// re-joined under another identity is waste, and only once its last holder
/// has let go.
fn collect_duplicate_now(marked_duplicate: bool, peer_refs: usize) -> bool {
    marked_duplicate && safe_to_retire(peer_refs)
}

thread_local! {
    /// Memberships owned by no client yet, keyed by the share's mesh key.
    ///
    /// The whole point of a dead-origin share is *waiting*: fofoca's own
    /// healing — a lone joiner's beacon claim, island merges, rendezvous
    /// failover — runs on cadences up to minutes, and a membership dropped
    /// after one bounded attempt loses every race and mints a ghost identity
    /// per retry. So the first attempt joins, and every later attempt reuses
    /// the same live membership: same identity, warm roster, and a tab that
    /// waits for new peers exactly the way a native `agent-share` process
    /// does. A successful connect moves the entry onto the client; an entry
    /// for a share the user navigated away from lives until the page closes,
    /// which is the meaning of "as long as the app is open, keep trying".
    static WAITING_MESHES: RefCell<HashMap<String, WaitingMesh>> =
        RefCell::new(HashMap::new());
}

/// How long the origin dial may run before the seeder fallback takes over.
///
/// A dial to a producer that is simply *gone* runs far longer than any
/// reconnect budget — the App measured 126 s against its 60 s window — so
/// without a cap the fallback below it is unreachable from a revival: the
/// caller's race kills the whole attempt while it is still inside the dial.
/// 30 s clears an honest slow connect — a live local dial was *measured*
/// taking ~20 s (JSEP + STUN + a cold relay handshake), and a 15 s cap cut
/// it off mid-handshake, sending a healthy share down the seeder path —
/// while still fitting the App's 60 s revival budget with the fallback's
/// own joins and dials behind it.
///
/// Applies only where a fallback exists (`dynamic`/`relay`); `webrtc` mode
/// pins the lane for tests and keeps failing at its own pace.
const ORIGIN_DIAL_CAP_MS: i32 = 30_000;

/// The honest cold dial this side has actually *measured*: JSEP, STUN, and a
/// cold relay handshake against a producer that is alive and answering. Any
/// cap this module picks for itself has to clear it, or a healthy origin is
/// unreachable by construction — see [`default_origin_cap_ms`].
const MEASURED_COLD_ORIGIN_DIAL_MS: i32 = 20_000;

const _: () = assert!(
    ORIGIN_DIAL_CAP_MS >= MEASURED_COLD_ORIGIN_DIAL_MS,
    "the patient origin cap must outlast a dial that was measured succeeding",
);

/// The cap for a connect whose caller named none: always the patient one.
///
/// A tab holding a manifest locator — proof it seeded this share before —
/// used to get 8 s here instead, on the theory that it serves itself from
/// its sidecar while the seeder lane races, so a long wait against a
/// probably-dead origin bought nothing. The theory had no way back. Nothing
/// ages or removes the locator, so the discount was permanent, and the
/// escape hatch it named ("the next retry reaches the origin again") did not
/// exist: every attempt re-read the same locator and re-applied the same 8 s.
/// A former seeder facing an origin that answers in the measured 6-20 s
/// therefore never connected — and if the browser evicted `IndexedDB` while
/// keeping `localStorage`, it took the discount without even having a
/// sidecar to serve from.
///
/// Deleting it costs the failure path alone. A seeder win does not wait for
/// the origin to give up (the lanes race, and `select` returns on the first
/// winner), and a seeder that answers now cuts the dial short on its own
/// evidence — see [`MeshCanServe`]. All the cap decides is how long a
/// doomed attempt takes to say so, and the App is retrying underneath a page
/// that already renders the persisted tree.
fn default_origin_cap_ms() -> i32 {
    ORIGIN_DIAL_CAP_MS
}

/// How long the origin dial runs alone before the seeder lane joins the race
/// (`dynamic` mode).
///
/// Long enough that a healthy origin — which answers in a second or two now
/// that ICE settles early — never sees a racing lane at all; short enough
/// that a dead origin costs three seconds rather than the whole
/// [`ORIGIN_DIAL_CAP_MS`] before card collection begins.
const SEEDER_RACE_HEAD_START_MS: i32 = 3_000;

/// How long the origin dial must have run before the seeder lane is allowed
/// to concede it. Protects a healthy share with a slow origin against a
/// seeder that answers instantly.
const ORIGIN_CONCEDE_FLOOR_MS: f64 = 5_000.0;

/// Proof that the mesh can serve this share, strong enough to cut a running
/// origin dial short.
///
/// Deliberately **not** raisable from a roster card. A departed peer's card
/// lives in the meta CRDT for the rest of the page's session — nothing
/// deletes it, see `ShareMeshDriver::refresh_book` — so "a card vouches" is
/// true on every attempt for any share that ever had a peer, ghosts
/// included. Conceding on that starved a live origin whose honest cold dial
/// was measured at 6-20 s: the dial died at [`ORIGIN_CONCEDE_FLOOR_MS`],
/// the race then dialled the ghosts, failed, and the App retried into the
/// same livelock. Only [`Self::peer_answered`] raises it, and only a peer
/// that actually answered a dial can trigger that.
#[derive(Clone, Default)]
struct MeshCanServe(Rc<Cell<bool>>);

impl MeshCanServe {
    /// A candidate's mount connection is up: someone alive is serving this
    /// share, so the origin no longer has to be waited out.
    ///
    /// The connection is taken as an argument it does not read, so that the
    /// rule above is the compiler's to keep rather than a comment's: a
    /// caller holding only a card has nothing to pass.
    fn peer_answered(&self, _proof: &Connection) {
        self.0.set(true);
    }

    fn proven(&self) -> bool {
        self.0.get()
    }

    /// The already-proven value, for tests that have no socket to dial.
    #[cfg(test)]
    fn already_answered() -> Self {
        let value = Self::default();
        value.0.set(true);
        value
    }
}

/// Whether the origin dial should concede now. Split out for the same
/// reason [`reoffer_due`] is: the rule is the whole safety argument, and a
/// rule that can be read on its own can be tested on its own.
fn concede_due(proven: bool, elapsed_ms: f64, floor_ms: f64) -> bool {
    proven && elapsed_ms >= floor_ms
}

/// Race an origin dial against [`ORIGIN_DIAL_CAP_MS`].
///
/// On timeout the dial future is dropped — its endpoints abort un-`close()`d,
/// which is acceptable for a producer we are about to give up on — and the
/// synthesized error routes the caller into the seeder fallback.
///
/// `concede` is the dynamic-mode lane coupling: the seeder lane raises its
/// [`MeshCanServe`] the moment a peer answers a dial, and this dial gives up
/// (after `floor_ms`) rather than running out its cap against an origin the
/// mesh has already replaced. Matters most when the seeder lane *fails*
/// after that — the caller then awaits this future, which now resolves in
/// one watcher tick instead of the cap's remainder.
/// Generic over the dial's winner for the same reason [`first_success`] is:
/// tests script the dial with a scalar instead of a [`ShareClient`].
async fn capped_origin_dial<Winner>(
    dial: std::pin::Pin<Box<dyn std::future::Future<Output = Result<Winner, JsValue>>>>,
    cap_ms: i32,
    concede: Option<(MeshCanServe, f64)>,
) -> Result<Winner, JsValue> {
    let concession = Box::pin(async move {
        let Some((can_serve, floor_ms)) = concede else {
            return std::future::pending::<()>().await;
        };
        let started = now_ms();
        loop {
            if concede_due(can_serve.proven(), now_ms() - started, floor_ms) {
                return;
            }
            wait_ms(250).await;
        }
    });
    let give_up = futures::future::select(Box::pin(wait_ms(cap_ms)), concession);
    match futures::future::select(dial, give_up).await {
        futures::future::Either::Left((dialed, _)) => dialed,
        futures::future::Either::Right((futures::future::Either::Left(((), _)), _)) => Err(
            JsValue::from_str(&format!("origin dial timed out after {} s", cap_ms / 1000)),
        ),
        futures::future::Either::Right((futures::future::Either::Right(((), _)), _)) => {
            Err(JsValue::from_str(
                "origin dial conceded: a peer on the mesh is already serving the share",
            ))
        }
    }
}

/// The origin is unreachable — stand the share up from its mesh instead.
///
/// Every holder of this link is on the mesh the ticket's token derives, so
/// join it first, then pick a peer whose card **vouches**: its `tree` matches
/// the majority tree among vouching cards (with the origin gone, agreement is
/// the only manifest authority left — guard #1 applied at selection), and it
/// advertises `serving`. Candidates are dialled over the relay lane — the
/// fallback that reaches a browser peer without an ICE round — native
/// (`unicast`) peers first, since they answer at line rate.
///
/// The manifest the winner serves is verified against the tree it advertised
/// before the client is handed to anyone; its answers stay a **frozen
/// snapshot** — seeders follow the origin while it lives and never mutate on
/// their own.
async fn connect_via_seeder(
    ticket: MountTicket,
    auth: ShareAuth,
    password: Option<&str>,
    card: Option<JsValue>,
    origin_error: &JsValue,
    can_serve: Option<MeshCanServe>,
) -> Result<ShareClient, JsValue> {
    let token = *auth.token();
    let secret = ticket.secret;
    let mesh_id = ticket.mesh_id.clone();
    let lookups = ticket.lookups.clone();
    let mesh_key = share_mesh_key(&token);

    // One membership per share, alive across attempts — see [`WAITING_MESHES`].
    let waiting = WAITING_MESHES.with(|meshes| meshes.borrow().get(&mesh_key).cloned());
    let waiting = match waiting {
        Some(entry) => entry,
        None => {
            // Minted before any client exists so the mesh join can register
            // the mount handler now; it lands on the client below, keeping
            // serving and advertising on the same handle.
            let seeder = seed::SeederShared::new();
            // Two endpoints, two hubs, and — deliberately — **two keys**.
            // The server half mirrors `produce.rs`: one relay-bearing
            // endpoint whose Router serves both ALPNs, sessions in its hub.
            // The client half gets its *own identity*: a seeder refuses a
            // second session per endpoint id (the one-registry rule), and
            // the mesh lane negotiates under the mesh id on its own schedule
            // — so a mount lane on the same key was refused or raced into
            // collisions every time. A distinct mount id makes the JSEP pair
            // unambiguous, immune to the mesh lane, and immune to the
            // opposite tab's simultaneous offers. Cost: a seeder's direct
            // count sees this tab twice (mesh id + mount id); honest, since
            // there really are two sessions.
            let key = SecretKey::generate();
            let local = key.public();
            let mesh_hub = BrowserHubTransport::new(local);
            let mesh_handle = WebRtcHandle::new(Arc::clone(&mesh_hub));
            let endpoint = Endpoint::builder(presets::Minimal)
                .secret_key(key)
                .relay_mode(relay_mode(&ticket.lookups.relay))
                .add_custom_transport(mesh_handle.transport())
                .path_selector(mesh_handle.path_selector())
                .bind()
                .await
                .map_err(|error| err("bind the waiting mesh endpoint", &error))?;
            let mount_key = SecretKey::generate();
            let mount_local = mount_key.public();
            let mount_hub = BrowserHubTransport::new(mount_local);
            let mount_handle = WebRtcHandle::new(Arc::clone(&mount_hub));
            // The JSEP half of the mount identity: relay-bearing, so offers
            // reach a seeder's signal ALPN, and carrying the mount id, so
            // its refusal check sees a fresh peer rather than the mesh
            // lane's session. See the field note on [`WaitingMesh`].
            let signal_endpoint = Endpoint::builder(presets::Minimal)
                .secret_key(mount_key.clone())
                .relay_mode(relay_mode(&ticket.lookups.relay))
                .bind()
                .await
                .map_err(|error| err("bind the mount-signal endpoint", &error))?;
            let mount_endpoint = Endpoint::builder(presets::Minimal)
                .secret_key(mount_key)
                .relay_mode(RelayMode::Disabled)
                .add_custom_transport(mount_handle.transport())
                .path_selector(mount_handle.path_selector())
                .bind()
                .await
                .map_err(|error| err("bind the seeder-mount endpoint", &error))?;
            let protocols: Vec<(Vec<u8>, Box<dyn fofoca::iroh::protocol::DynProtocolHandler>)> = vec![
                (
                    MOUNT_ALPN.to_vec(),
                    Box::new(produce::MountHandler::new(seeder.clone(), auth)),
                ),
                (
                    WEBRTC_SIGNAL_ALPN.to_vec(),
                    Box::new(produce::SignalHandler::new(local, Arc::clone(&mesh_hub))),
                ),
            ];
            let card_parts = match card.as_ref() {
                Some(value) => {
                    mesh::parse_card_parts(value, "webrtc", Some("consumer".to_owned()))?
                }
                None => mesh::default_card_parts("webrtc", Some("consumer".to_owned())),
            };
            let resolved = mesh::resolve_share(mesh::ShareMeshRef {
                mesh_id: mesh_id.as_deref(),
                secret: &secret,
                lookups: &lookups,
                password,
            })?;
            let peer = mesh::MeshPeer::join_share(
                resolved,
                Some((endpoint.clone(), mesh_handle)),
                protocols,
                card_parts,
            )
            .await
            .map_err(|mesh_error| {
                JsValue::from_str(&format!(
                    "the origin is unreachable ({}) and the share's mesh could not be \
                     joined ({})",
                    describe(origin_error),
                    describe(&mesh_error),
                ))
            })?;
            let entry = WaitingMesh {
                peer: Rc::new(peer),
                seeder,
                endpoint,
                signal_endpoint,
                mount_endpoint,
                mount_hub,
                mesh_hub,
                duplicate: Rc::new(Cell::new(false)),
            };
            // Another attempt for this share may have finished minting
            // while this one was binding its endpoints. Whoever lost says
            // goodbye properly instead of being dropped on the floor.
            match WAITING_MESHES.with(|meshes| claim_waiting_mesh(meshes, &mesh_key, entry.clone()))
            {
                Some(winner) => {
                    web_sys::console::log_1(&JsValue::from_str(
                        "[share] a concurrent attempt already joined this share's mesh; \
                         retiring the duplicate membership",
                    ));
                    entry.retire(true).await;
                    winner
                }
                None => entry,
            }
        }
    };
    let mesh_peer = &waiting.peer;
    let seeder = &waiting.seeder;

    // A tab that seeded here before re-arms from its own storage *before*
    // asking anyone: the manifest sidecar plus the blob store are enough to
    // serve and to vouch. This is what breaks the everyone-refreshed
    // deadlock — two re-armed tabs are each other's source, and a share can
    // stand back up with no live source anywhere. Skipped once the seeder
    // already carries a tree (a reused membership re-armed on an earlier
    // attempt, or fed by a previous session's sync).
    {
        use agent_share_mount::ServeSource as _;
        if seeder.manifest_envelope().is_none()
            && let Some((bytes, tree, manifest, store)) = load_persisted_manifest(&token).await
        {
            let rows = rows_in_store(&store, &manifest).await;
            web_sys::console::debug_1(&JsValue::from_str(&format!(
                "[share] re-arm: {} rows recovered from {} manifest slots",
                rows.len(),
                manifest.files.len()
            )));
            if !rows.is_empty() {
                // Serving before advertising, as everywhere else — and it has to
                // come first here anyway, since the held set is now asked of the
                // seeder rather than walked separately.
                seeder.update(Arc::new(bytes), rows, store);
                // Only complete slots go on the card, which is a *discovery*
                // hint: partial holdings are real and servable, but a reader
                // learns about them by asking, not from a CRDT that would keep
                // every intermediate state forever.
                let held = seeder.complete_slots().await;
                // `tree` came out of the locator, which records the fingerprint
                // over the manifest and not the envelope beside it — already
                // checked against these bytes on the way in, so re-deriving it
                // here would re-encode a multi-MB manifest to reach the same
                // string.
                let serving =
                    agent_share_proto::serving::encode_serving(&held, manifest.files.len());
                web_sys::console::debug_1(&JsValue::from_str(&format!(
                    "[share] re-arm: advertising tree {tree} serving {serving:?}                      ({} slots held)",
                    held.len()
                )));
                mesh_peer.set_tree(tree).await;
                mesh_peer.set_serving(serving).await;
            }
        }
    }

    // Before waiting on gossip at all, try the seeders this tab vetted on an
    // earlier connect. When they are still alive this stands the share back
    // up in seconds while a beacon-less mesh is still healing; when they are
    // corpses the lane is bounded and prunes them, and the card path below
    // proceeds unchanged.
    if let Some(client) =
        redial_known_seeders(&waiting, &ticket, &token, origin_error, can_serve.as_ref()).await
    {
        return Ok(client);
    }

    // Cards arrive over gossip; poll until somebody vouches or the deadline.
    // The poll count is what keeps this honest in a hidden tab, where the
    // deadline can pass inside a single clamped tick — see [`out_of_time`].
    let started = now_ms();
    let mut polls = 0u32;
    let vouching = loop {
        let vouching: Vec<agent_share_proto::PeerCard> = mesh_peer
            .known_cards()
            .into_iter()
            .filter(|card| {
                card.tree.is_some()
                    && card.serving.is_some()
                    && card.endpoint != mesh_peer.hub().local_id().to_string()
            })
            .collect();
        if !vouching.is_empty() {
            // Note what is *not* here: the racing origin dial is not told to
            // concede. A card is a claim by a peer that may have closed its
            // tab an hour ago — see [`MeshCanServe`] — so the proof is
            // deferred to the dial that actually reaches one of them.
            break vouching;
        }
        polls += 1;
        if out_of_time(now_ms() - started, polls, SEEDER_CARDS_DEADLINE_MS) {
            let detail = format!(
                "the origin is unreachable ({}) and no peer on the mesh vouches for the share",
                describe(origin_error),
            );
            // No password hedge here any more. A protected ticket that carries
            // a mesh id had its password ruled on locally before the dial, so
            // reaching this point means the password was *right* and the share
            // is simply unreachable. Blaming the password would be a false
            // accusation, and it used to be one.
            //
            // The exception is a protected ticket with no mesh id — minted
            // before that field existed — where nothing local could rule, so the
            // password genuinely remains a candidate.
            return Err(if auth.password_protected() && mesh_id.is_none() {
                unauthorized(&format!("{detail} — the password may also be wrong"))
            } else {
                JsValue::from_str(&detail)
            });
        }
        wait_ms(500).await;
    };

    let majority = majority_tree(&vouching).expect("vouching cards all carry a tree");

    let mut candidates: Vec<&agent_share_proto::PeerCard> = vouching
        .iter()
        .filter(|card| card.tree.as_deref() == Some(majority.as_str()))
        .collect();
    // Native peers first: they serve at line rate and are reachable without
    // any browser in the path. Within a class the order is random on
    // purpose: ghost cards from departed peers survive on the roster, and a
    // fixed order would march every attempt through the same ghosts ahead
    // of the live seeder — random sampling lets a retry land on it. Cached
    // keys, or the comparator would re-roll mid-sort.
    candidates.sort_by_cached_key(|card| {
        (
            card.transport != "unicast",
            js_sys::Math::random().to_bits(),
        )
    });

    let relays: Vec<TransportAddr> = seeder_relays(&ticket)
        .into_iter()
        .map(TransportAddr::Relay)
        .collect();
    if relays.is_empty() {
        return Err(JsValue::from_str(
            "this ticket names no relay, and a tab has no other way to reach a seeder",
        ));
    }

    // Ghost cards from departed peers cannot answer a dial but cost a full
    // budget finding out — the JSEP wait alone is `SEEDER_CHANNEL_WAIT_MS`,
    // and the roster can hold dozens of ghosts per live seeder. Serially
    // that was minutes of "connecting" against a share that was up, so the
    // attempts race: staggered launches keep the native-first preference
    // (an earlier candidate that answers promptly wins before a later one
    // even starts), the width caps the JSEP and relay fan-out, and the
    // deadline bounds the whole attempt. The caller retries forever, and
    // every retry re-samples the roster.
    let mut refusals = Vec::new();
    let waiting_ref = &waiting;
    let relays_ref = &relays;
    let terms = VetTerms {
        token: &token,
        author: ticket.author,
        tree: majority.as_str(),
        channel_wait: f64::from(SEEDER_CHANNEL_WAIT_MS),
        can_serve: can_serve.as_ref(),
    };
    let mut attempts: Vec<
        std::pin::Pin<Box<dyn std::future::Future<Output = Result<VettedSeeder, Refusal>> + '_>>,
    > = Vec::new();
    for (slot, candidate) in candidates.iter().take(SEEDER_RACE_WIDTH).enumerate() {
        let Ok(id) = candidate
            .endpoint
            .parse::<fofoca::protocol::iroh_base::EndpointId>()
        else {
            continue;
        };
        let endpoint = candidate.endpoint.clone();
        let delay = i32::try_from(slot).unwrap_or(0) * SEEDER_RACE_STAGGER_MS;
        attempts.push(Box::pin(async move {
            if delay > 0 {
                wait_ms(delay).await;
            }
            vet_seeder_candidate(waiting_ref, endpoint, id, relays_ref, &terms).await
        }));
    }
    if attempts.is_empty() {
        return Err(JsValue::from_str(&format!(
            "the origin is unreachable ({}) and no vouching card carries a dialable endpoint",
            describe(origin_error),
        )));
    }
    if candidates.len() > attempts.len() {
        web_sys::console::log_1(&JsValue::from_str(&format!(
            "[share] racing {} of {} candidates; the rest wait for the next attempt",
            attempts.len(),
            candidates.len(),
        )));
    }
    let vetted = match first_success(
        attempts,
        Box::pin(wait_ms(SEEDER_RACE_DEADLINE_MS)),
        &mut refusals,
    )
    .await
    {
        RaceOutcome::Winner(vetted) => vetted,
        RaceOutcome::AllFailed => {
            return Err(JsValue::from_str(&format!(
                "the origin is unreachable ({}) and no seeder could serve the share: {}",
                describe(origin_error),
                refusal_lines(&refusals),
            )));
        }
        RaceOutcome::DeadlineExpired => {
            return Err(JsValue::from_str(&format!(
                "the origin is unreachable ({}) and no seeder answered inside {} s: {}",
                describe(origin_error),
                SEEDER_RACE_DEADLINE_MS / 1_000,
                refusal_lines(&refusals),
            )));
        }
    };

    // The winner earned its record: the next reconnect redials it directly
    // instead of waiting for gossip to reintroduce it.
    remember_known_seeder(&token, &vetted.endpoint, &majority);
    Ok(adopt_vetted(
        vetted,
        &waiting,
        token,
        ticket.author,
        lookups,
        majority.as_str(),
        origin_error,
    )
    .await)
}

/// The redial lane's whole budget. One channel try plus the relay fallback
/// per candidate must fit inside it; sized so a roster of corpses delays
/// the card path by at most this before being pruned.
const KNOWN_SEEDER_REDIAL_DEADLINE_MS: i32 = vet_floor_ms(KNOWN_SEEDER_CHANNEL_WAIT_MS);

const _: () = assert!(
    KNOWN_SEEDER_REDIAL_DEADLINE_MS >= vet_floor_ms(KNOWN_SEEDER_CHANNEL_WAIT_MS),
    "the redial lane must at least reach and verify one candidate",
);

/// The channel wait inside the redial lane — much tighter than
/// [`SEEDER_CHANNEL_WAIT_MS`], so the webrtc try and the relay fallback
/// both fit the lane's deadline.
const KNOWN_SEEDER_CHANNEL_WAIT_MS: i32 = 6_000;

/// Launch spacing inside the redial lane; tighter than the card race's
/// because there are at most [`KNOWN_SEEDERS_CAP`] candidates and every one
/// was live recently.
const KNOWN_SEEDER_STAGGER_MS: i32 = 500;

/// Redial the seeders this tab vetted on an earlier connect, straight from
/// `localStorage`, skipping the card wait. A reviving tab already knows who
/// served it, and the bytes plane never needed the mesh's beacon — only
/// discovery does — so when those peers are still alive this lane connects
/// while a producer-less mesh is still healing its rendezvous.
///
/// Every safeguard the card path applies still runs per candidate: the
/// manifest must hash to the *recorded* tree (a stale record is refused,
/// and the card path's majority vote takes over), and a data-channel win is
/// bulk-probed with relay demotion. `None` means fall through to the card
/// path; every endpoint dialled without a win is forgotten first.
async fn redial_known_seeders(
    waiting: &WaitingMesh,
    ticket: &MountTicket,
    token: &[u8; SECRET_LEN],
    origin_error: &JsValue,
    can_serve: Option<&MeshCanServe>,
) -> Option<ShareClient> {
    let entries = load_known_seeders(token);
    if entries.is_empty() {
        return None;
    }
    let author = ticket.author;
    let relays: Vec<TransportAddr> = seeder_relays(ticket)
        .into_iter()
        .map(TransportAddr::Relay)
        .collect();
    if relays.is_empty() {
        return None;
    }
    // A same-session revival reuses the waiting membership's identity, so
    // this tab's own record — it may have vouched for itself — is skippable
    // by id rather than by luck.
    let own = waiting.peer.hub().local_id().to_string();
    let relays_ref = &relays;
    let mut attempts: Vec<
        std::pin::Pin<Box<dyn std::future::Future<Output = Result<VettedSeeder, Refusal>> + '_>>,
    > = Vec::new();
    for entry in entries.iter().filter(|entry| entry.endpoint != own) {
        let Ok(id) = entry
            .endpoint
            .parse::<fofoca::protocol::iroh_base::EndpointId>()
        else {
            continue;
        };
        let endpoint = entry.endpoint.clone();
        let tree = entry.tree.clone();
        let delay = i32::try_from(attempts.len()).unwrap_or(0) * KNOWN_SEEDER_STAGGER_MS;
        attempts.push(Box::pin(async move {
            if delay > 0 {
                wait_ms(delay).await;
            }
            let terms = VetTerms {
                token,
                author,
                tree: &tree,
                channel_wait: f64::from(KNOWN_SEEDER_CHANNEL_WAIT_MS),
                can_serve,
            };
            vet_seeder_candidate(waiting, endpoint, id, relays_ref, &terms).await
        }));
    }
    if attempts.is_empty() {
        return None;
    }
    let mut refusals = Vec::new();
    match first_success(
        attempts,
        Box::pin(wait_ms(KNOWN_SEEDER_REDIAL_DEADLINE_MS)),
        &mut refusals,
    )
    .await
    {
        RaceOutcome::Winner(vetted) => {
            web_sys::console::log_1(&JsValue::from_str(&format!(
                "[share] known seeder {} answered ahead of the card wait",
                &vetted.endpoint[..8.min(vetted.endpoint.len())],
            )));
            let tree = agent_share_proto::manifest::manifest_fingerprint(&vetted.fetched.body);
            // A win is the strongest proof of life there is, so any misses
            // this endpoint had banked from earlier attempts are void. The
            // strike count has to reset on evidence or it only ever climbs,
            // and a peer that answers most of the time would still be
            // deleted eventually.
            let cleared = roster_after_failed_lane(
                load_known_seeders(token),
                &[Refusal {
                    endpoint: vetted.endpoint.clone(),
                    reason: String::new(),
                    unreached: false,
                }],
            );
            store_known_seeders(token, &cleared);
            // Note what is *not* here: a `remember_known_seeder` call. Its
            // fresh `seen_ms` would restart the 24 h TTL, and a record that
            // renews itself every time it wins can never expire — a seeder
            // frozen on an outdated tree would be re-adopted, re-recorded and
            // re-adopted for the life of the tab, with the majority vote it
            // bypasses never getting a turn. Leaving the timestamp alone
            // turns the TTL into what it reads as: how long this tab trusts
            // its own memory before making the mesh vote again.
            let endpoint = vetted.endpoint.clone();
            let client = adopt_vetted(
                vetted,
                waiting,
                *token,
                author,
                ticket.lookups.clone(),
                &tree,
                origin_error,
            )
            .await;
            challenge_adopted_tree(waiting, *token, endpoint, tree);
            Some(client)
        }
        RaceOutcome::AllFailed | RaceOutcome::DeadlineExpired => {
            let kept = roster_after_failed_lane(load_known_seeders(token), &refusals);
            store_known_seeders(token, &kept);
            if !refusals.is_empty() {
                web_sys::console::log_1(&JsValue::from_str(&format!(
                    "[share] known-seeder redial found nobody home ({}); waiting for cards",
                    refusal_lines(&refusals),
                )));
            }
            None
        }
    }
}

/// How long to keep asking the roster about a tree the redial lane pinned.
/// Long enough to outlast the card wait that the lane skipped, since that is
/// how long cards were budgeted to take in the first place.
const TREE_CHALLENGE_WINDOW_MS: f64 = 45_000.0;

/// Let the mesh contradict a tree the redial lane pinned without a vote.
///
/// The lane's speed comes from trusting this tab's own record instead of
/// waiting for cards, so the cards arrive after the client is already
/// serving. When they disagree, the record that led us here is the thing at
/// fault: forget it, and the next connect takes the card path and votes.
///
/// It deliberately stops there. Tearing the connection down on a card count
/// would hand every departed peer a vote to disconnect live tabs with —
/// ghosts hold whichever tree they last published, so a share that moved on
/// leaves a ghost majority for the *old* tree behind it. Dropping a record
/// costs one card wait if the vote was wrong; killing a mount on the same
/// evidence would cost a working share.
fn challenge_adopted_tree(
    waiting: &WaitingMesh,
    token: [u8; SECRET_LEN],
    endpoint: String,
    pinned: String,
) {
    // Weak on purpose. A strong clone held for the whole window would count
    // as a live holder to [`safe_to_retire`], quietly postponing the
    // origin-win cleanup for as long as this watcher runs — and a watcher
    // outliving the membership it watches has nothing left to say anyway.
    let peer = Rc::downgrade(&waiting.peer);
    wasm_bindgen_futures::spawn_local(async move {
        let started = now_ms();
        while now_ms() - started < TREE_CHALLENGE_WINDOW_MS {
            wait_ms(1_000).await;
            let Some(peer) = peer.upgrade() else {
                return;
            };
            let own = peer.hub().local_id().to_string();
            match judge_adopted_tree(&peer.known_cards(), &own, &pinned) {
                TreeVerdict::Unknown => continue,
                TreeVerdict::Agrees => return,
                TreeVerdict::Disagrees => {
                    web_sys::console::log_1(&JsValue::from_str(&format!(
                        "[share] the mesh serves a different tree than seeder {} did; \
                         forgetting it so the next connect votes",
                        &endpoint[..8.min(endpoint.len())],
                    )));
                    forget_known_seeders(&token, std::slice::from_ref(&endpoint));
                    return;
                }
            }
        }
    });
}

/// Graduate a race's winner into a [`ShareClient`] homed on the waiting
/// membership. Shared by the known-seeder redial lane and the card race —
/// the winner is adopted identically no matter which lane vetted it.
async fn adopt_vetted(
    vetted: VettedSeeder,
    waiting: &WaitingMesh,
    token: [u8; SECRET_LEN],
    author: Option<[u8; 32]>,
    lookups: LookupOpts,
    majority: &str,
    origin_error: &JsValue,
) -> ShareClient {
    // Report the wire, not the intent: the settled path says whether the
    // channel or the relay carried the win — except a demotion, which
    // already knows it rides the relay.
    let data_path = if vetted.demoted {
        "relay".to_owned()
    } else {
        settled_path_label(&vetted.connection)
            .await
            .unwrap_or_else(|| "relay".to_owned())
    };
    // A relay-carried win stays on the relay for this connection's whole
    // life. There is no upgrade watcher — none was ever built, here or
    // anywhere (the module doc explains why in-place upgrade is impossible;
    // upgrading means redialling) — so the lane is only re-evaluated when a
    // natural reconnect runs the webrtc-first dial again. If relay wins
    // ever need to be temporary, that is a new upgrade-by-redial task:
    // scope it by pairing (only browser↔browser bulk ever stalled — see
    // `probe_read`) and reuse this membership's hubs, per the comment below.
    let short = vetted.endpoint[..8.min(vetted.endpoint.len())].to_owned();
    let mut client = new_share_client(
        vetted.connection,
        token,
        author,
        data_path,
        Some(Arc::clone(&waiting.mount_hub)),
        None,
        None,
        if vetted.demoted {
            waiting.endpoint.clone()
        } else {
            waiting.mount_endpoint.clone()
        },
    );
    client.lookups = lookups;
    client.connected_at_ms = now_ms();
    client.from_origin = false;
    client.pinned_tree = Some(majority.to_owned());
    *client.fallback_reason.borrow_mut() = Some(if vetted.demoted {
        format!(
            "origin unreachable ({}); reading from seeder {short} over the relay (data channel failed the bulk probe)",
            describe(origin_error),
        )
    } else {
        format!(
            "origin unreachable ({}); reading from seeder {short}",
            describe(origin_error),
        )
    });
    client.seeder = waiting.seeder.clone();
    // The membership stays in the registry even as it graduates onto the
    // client: its hubs hold the live sessions, and any revival — a future
    // upgrade-by-redial included — must reuse them, because a fresh
    // identity per redial would drop every channel it just built.
    // `leave_mesh` finally purges it.
    *client.mesh.borrow_mut() = MeshSlot::Joined(Rc::clone(&waiting.peer));
    // The vetting fetch already paid for these bytes; the first manifest
    // call reuses them instead of re-paying the RTT.
    *client.prefetched_manifest.borrow_mut() = Some(vetted.fetched);
    client
}

/// Reach `seeder`'s mount over the data channel: reuse the session the hub
/// already holds, else run one JSEP round (offer over the relay-bearing
/// endpoint, attach into the mount hub), then dial the mount ALPN from the
/// relay-free endpoint at an address carrying only the custom addr — so the
/// connection cannot settle anywhere but the channel.
/// How long to wait for a data channel to `seeder` before conceding relay.
///
/// Two lanes race to build one: our own JSEP round below, and fofoca's mesh
/// negotiation, which retries on its own schedule. A session was once
/// *measured* landing a minute after two freshly-reloaded tabs meet, but
/// that predates fofoca's beacon-failover fixes; conceding to the relay at
/// 15 s keeps the attempt moving, and the App's retry redials webrtc. Do
/// not cut further without drill data: a demotion to relay is permanent for
/// the connection, so a too-sharp wait converts webrtc wins into relay
/// sessions.
const SEEDER_CHANNEL_WAIT_MS: i32 = MEASURED_CHANNEL_FORMATION_MS;

const _: () = assert!(
    SEEDER_CHANNEL_WAIT_MS >= MEASURED_CHANNEL_FORMATION_MS,
    "conceding the relay before a session forms makes the relay permanent",
);

/// What channel formation was *measured* needing, rather than hoped to need.
///
/// fofoca's mesh negotiation was measured landing a session a minute after
/// two freshly-reloaded tabs meet, and this repo's own drill records the
/// both-tabs-reloaded byte lane at ~96 s. 30 s is the value that lane ran on
/// before it was halved on the theory that beacon-failover fixes had covered
/// it — but those fixes govern rendezvous, not JSEP, and no drill was run.
const MEASURED_CHANNEL_FORMATION_MS: i32 = 30_000;

/// Whether another JSEP offer is due: one at entry, one more once half the
/// wait has passed with no session — STUN can lose a round transiently, and
/// one retry was the difference measured. Keeping the re-offer at the
/// halfway mark by construction lets the wait shrink without re-deriving
/// the schedule.
fn reoffer_due(offers: u32, elapsed: f64, wait: f64) -> bool {
    match offers {
        0 => true,
        1 => elapsed > wait / 2.0,
        _ => false,
    }
}

/// The three handles a peer dial needs, wherever the caller keeps them.
///
/// A parameter rather than a `&WaitingMesh` because the registry entry is not
/// always there to borrow: a client that reached the origin **consumes** its
/// waiting membership at connect and carries the pieces on itself. Taking the
/// pieces lets a live client dial too, which is what the swarm does.
#[derive(Clone, Copy)]
struct DialLanes<'a> {
    /// The mount identity's relay-bearing half, for JSEP only — a seeder's
    /// refusal is keyed to the TLS-proven id of the *signal* connection.
    signal_endpoint: &'a Endpoint,
    /// Relay-free, so the connection can settle nowhere but the channel.
    mount_endpoint: &'a Endpoint,
    mount_hub: &'a Arc<BrowserHubTransport>,
}

impl WaitingMesh {
    fn lanes(&self) -> DialLanes<'_> {
        DialLanes {
            signal_endpoint: &self.signal_endpoint,
            mount_endpoint: &self.mount_endpoint,
            mount_hub: &self.mount_hub,
        }
    }
}

async fn seeder_webrtc_dial(
    waiting: DialLanes<'_>,
    seeder: fofoca::protocol::iroh_base::EndpointId,
    relays: &[TransportAddr],
    wait: f64,
) -> Result<Connection, JsValue> {
    let webrtc_only =
        EndpointAddr::from_parts(seeder, [TransportAddr::Custom(custom_addr(seeder))]);

    // The mount lane runs under its own identity, so its JSEP rounds cannot
    // be refused for the mesh lane's session nor collide with the opposite
    // tab's offers. Offer, wait for the session, re-offer once if ICE lost a
    // round, then dial from the relay-free endpoint — where the channel is
    // the only path a connection can settle on.
    let started = now_ms();
    let mut offers = 0u32;
    let mut polls = 0u32;
    loop {
        if waiting.mount_hub.has_session(&seeder) {
            return waiting
                .mount_endpoint
                .connect(webrtc_only, MOUNT_ALPN)
                .await
                .map_err(|error| {
                    err("dial the mount ALPN over the seeder's data channel", &error)
                });
        }
        if reoffer_due(offers, now_ms() - started, wait) {
            offers += 1;
            let addr = EndpointAddr::from_parts(seeder, relays.iter().cloned());
            match negotiate(
                waiting.signal_endpoint,
                addr,
                waiting.mount_hub.local_id(),
                &waiting.mount_hub,
            )
            .await
            {
                Ok(_) => web_sys::console::log_1(&JsValue::from_str(&format!(
                    "[share] seeder JSEP round {offers} attached"
                ))),
                Err(error) => web_sys::console::log_1(&JsValue::from_str(&format!(
                    "[share] seeder JSEP round {offers} failed: {}",
                    describe(&error)
                ))),
            }
        }
        // `negotiate` has no timeout of its own — it blocks on reading the
        // answer — so it routinely returns *after* the wait has run out, and
        // sometimes it returns having attached. Re-asking is the difference
        // between taking that session and throwing away a data channel that
        // exists, since the loop below would concede to the relay and a
        // relay win is permanent for the connection's life.
        if waiting.mount_hub.has_session(&seeder) {
            continue;
        }
        polls += 1;
        if out_of_time(now_ms() - started, polls, wait) {
            return Err(JsValue::from_str(
                "no data channel formed inside the wait; conceding to the relay",
            ));
        }
        wait_ms(1_000).await;
    }
}

/// How many seeder candidates one attempt dials at once. Enough that a
/// handful of ghost cards cannot monopolize an attempt, small enough that
/// the JSEP and relay fan-out stays polite; candidates past the width wait
/// for the caller's next retry, which re-samples the roster.
const SEEDER_RACE_WIDTH: usize = 6;

/// Launch spacing inside the race. The stagger is what preserves the
/// native-first sort as a *preference*: an earlier candidate that answers
/// promptly wins before a later one has started, while a dead one only
/// costs the race this delay instead of its whole budget.
const SEEDER_RACE_STAGGER_MS: i32 = 2_000;

/// The whole attempt's bound. Sized past one full JSEP wait
/// (`SEEDER_CHANNEL_WAIT_MS`) plus a relay fallback plus a bulk-probe stall
/// (`PROBE_STALL_MS`) with margin, so the first candidate is never cut
/// short — and low enough that a roster full of ghosts hands control back
/// to the forever-retrying caller in about half a minute. The caller's card
/// poll (`SEEDER_CARDS_DEADLINE_MS`) plus this is the ceiling on one silent
/// "connecting" stretch.
const SEEDER_RACE_DEADLINE_MS: i32 = vet_budget_ms(SEEDER_CHANNEL_WAIT_MS);

const _: () = assert!(
    SEEDER_RACE_DEADLINE_MS >= vet_budget_ms(SEEDER_CHANNEL_WAIT_MS),
    "the card race must let its first candidate finish a whole vet",
);

/// Everything the winning candidate hands back: a vetted connection plus
/// the manifest bytes the vetting already paid for.
struct VettedSeeder {
    connection: Connection,
    /// The candidate's endpoint string, for user-facing messages.
    endpoint: String,
    /// Already signature-checked: vetting is a real manifest fetch, so a
    /// candidate that could not prove authorship never became a `VettedSeeder`.
    fetched: FetchedManifest,
    /// The data channel failed the bulk probe and `connection` is the relay
    /// replacement: label it "relay" and home the client on the
    /// relay-bearing endpoint.
    demoted: bool,
}

/// Why a candidate did not serve this attempt, and the part the roster cares
/// about: whether the dial ever reached it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Refusal {
    endpoint: String,
    /// The line for the attempt log.
    reason: String,
    /// Neither the data channel nor the relay produced a connection.
    ///
    /// Only this says anything about whether the peer is still out there.
    /// Every later failure happens *after* a connection landed, so it is
    /// about the share — a tree that moved, a stalled read — and not about
    /// the peer's existence.
    unreached: bool,
}

/// What a candidate is held to, and what it raises when it passes. Bundled
/// because both lanes — the known-seeder redial and the card race — vet
/// against the same four things and differ only in their values.
#[derive(Clone, Copy)]
struct VetTerms<'a> {
    token: &'a [u8; SECRET_LEN],
    /// The ticket's authorship key. When it is set, a candidate is judged on
    /// the creator's signature and the card majority below is not consulted —
    /// counting gossiped cards is a vote a Sybil wins, and a signature settles
    /// the same question without one.
    author: Option<[u8; 32]>,
    /// The tree the candidate must serve: the card majority, or the tree
    /// this tab recorded for a known seeder. Only consulted for an unsigned
    /// share, which has no offline authority at all.
    tree: &'a str,
    /// How long to wait for a data channel before conceding the relay.
    channel_wait: f64,
    /// Raised when the candidate's connection lands, if the caller is
    /// racing an origin dial that wants to know.
    can_serve: Option<&'a MeshCanServe>,
}

/// One candidate, dialled and vetted end to end. WebRTC first — the whole
/// point of a swarm of browsers is that bytes flow tab-to-tab, not through
/// the relay, which stays the honest fallback lane. The manifest is fetched
/// and hashed against the card's claim, and a data-channel win is
/// bulk-probed and demoted to a fresh relay connection if it stalls. `Err`
/// is the refusal line for the attempt log.
async fn vet_seeder_candidate(
    waiting: &WaitingMesh,
    endpoint: String,
    id: fofoca::protocol::iroh_base::EndpointId,
    relays: &[TransportAddr],
    terms: &VetTerms<'_>,
) -> Result<VettedSeeder, Refusal> {
    let VetTerms {
        token,
        author,
        tree,
        channel_wait,
        can_serve,
    } = *terms;
    let short = endpoint[..8.min(endpoint.len())].to_owned();
    // Which arm won decides whether the bulk probe below runs: only the
    // data channel is suspect, and provenance says it more cheaply and more
    // precisely than re-reading the settled path.
    let (connection, via_data_channel) =
        match seeder_webrtc_dial(waiting.lanes(), id, relays, channel_wait).await {
            Ok(connection) => (connection, true),
            Err(webrtc_error) => {
                let addr = EndpointAddr::from_parts(id, relays.iter().cloned());
                match waiting.endpoint.connect(addr, MOUNT_ALPN).await {
                    Ok(connection) => (connection, false),
                    Err(error) => {
                        return Err(Refusal {
                            endpoint,
                            reason: format!(
                                "{short}: webrtc: {}; relay: {error}",
                                describe(&webrtc_error),
                            ),
                            unreached: true,
                        });
                    }
                }
            }
        };
    // A mount connection to a peer that is not us: the mesh demonstrably
    // holds this share, which is the only evidence that may cut the origin
    // dial short. Raised here rather than on the card that named this
    // candidate, because a card outlives the tab that published it.
    if let Some(can_serve) = can_serve {
        can_serve.peer_answered(&connection);
    }
    // The candidate must serve the tree its card claimed — fetched bytes,
    // hashed here, against the card. A mismatch is disqualifying, not
    // retryable: it lied once.
    let fetched = match fetch_manifest_on(&connection, token, author).await {
        // Signed: the fetch already checked the creator's signature, so the
        // card's claim adds nothing and is deliberately not consulted.
        Ok(fetched) if author.is_some() => fetched,
        Ok(fetched)
            if agent_share_proto::manifest::manifest_fingerprint(&fetched.body) == tree =>
        {
            fetched
        }
        Ok(_) => {
            return Err(Refusal {
                endpoint,
                reason: format!("{short}: served a different tree than its card claimed"),
                unreached: false,
            });
        }
        Err(error) => {
            return Err(Refusal {
                endpoint,
                reason: format!("{short}: {}", describe(&error)),
                unreached: false,
            });
        }
    };
    // Prove the path moves bulk before trusting it with the share; see
    // `probe_read`. Only a data-channel connection is suspect — bulk never
    // stalled on the relay or a direct ip, so probing those lanes would
    // only gate healthy seeders behind a timer. A webrtc mount that stalls
    // is closed and redialled over the relay — bytes beat purity — and that
    // replacement is trusted the same way any relay connection is.
    let target = if via_data_channel {
        let target = probe_target(&fetched.manifest.files);
        if target.is_none() {
            // Rare enough to say out loud: an unprobed channel carrying a
            // manifest with no readable bytes is fine today, but silence
            // here would read as "probed and passed" in a log.
            web_sys::console::log_1(&JsValue::from_str(
                "[share] bulk probe skipped: the manifest holds no readable bytes",
            ));
        }
        target
    } else {
        None
    };
    if let Some((index, want)) = target
        && let Err(probe_error) = probe_read(&connection, token, index, want).await
    {
        web_sys::console::log_1(&JsValue::from_str(&format!(
            "[share] seeder path failed the bulk probe ({}); trying the relay",
            describe(&probe_error)
        )));
        connection.close(0u32.into(), b"failed the bulk probe");
        let addr = EndpointAddr::from_parts(id, relays.iter().cloned());
        return match waiting.endpoint.connect(addr, MOUNT_ALPN).await {
            Ok(relay_conn) => Ok(VettedSeeder {
                connection: relay_conn,
                endpoint,
                fetched,
                demoted: true,
            }),
            Err(error) => Err(Refusal {
                endpoint,
                reason: format!("{short}: relay redial after failed probe: {error}"),
                unreached: false,
            }),
        };
    }
    Ok(VettedSeeder {
        connection,
        endpoint,
        fetched,
        demoted: false,
    })
}

/// How a candidate race ended; [`first_success`] is the driver.
enum RaceOutcome<Winner> {
    Winner(Winner),
    /// Every attempt returned a refusal.
    AllFailed,
    /// The deadline fired with attempts still in flight.
    DeadlineExpired,
}

/// Drive `attempts` until one succeeds, every one fails, or `deadline`
/// fires. Refusals land in `refusals` as they happen; a win drops the
/// still-running attempts, which cancels them mid-dial — the same
/// cancellation an abandoned connect always had. A seam like
/// [`drain_with_stall_deadline`]: generic so tests can script the futures.
async fn first_success<Winner, Refused, Attempt, Deadline>(
    mut pending: Vec<Attempt>,
    mut deadline: Deadline,
    refusals: &mut Vec<Refused>,
) -> RaceOutcome<Winner>
where
    Attempt: std::future::Future<Output = Result<Winner, Refused>> + Unpin,
    Deadline: std::future::Future<Output = ()> + Unpin,
{
    loop {
        if pending.is_empty() {
            return RaceOutcome::AllFailed;
        }
        match futures::future::select(futures::future::select_all(pending), &mut deadline).await {
            futures::future::Either::Left(((outcome, _which, rest), _)) => match outcome {
                Ok(winner) => return RaceOutcome::Winner(winner),
                Err(refusal) => {
                    refusals.push(refusal);
                    pending = rest;
                }
            },
            futures::future::Either::Right(((), _)) => return RaceOutcome::DeadlineExpired,
        }
    }
}

/// The relay rungs a seeder of this share can be dialled at.
///
/// Every peer of a share homes on the ladder the ticket names — the same
/// rungs the mesh rendezvous uses — so the ladder, not any one URL, is the
/// address half of "dial by endpoint id".
fn seeder_relays(ticket: &MountTicket) -> Vec<fofoca::iroh::RelayUrl> {
    use agent_share_proto::lookup::RelayChoice;
    match &ticket.lookups.relay {
        RelayChoice::Disabled => Vec::new(),
        RelayChoice::Pinned => pinned_ladder(),
        RelayChoice::Custom(ladder) => ladder.clone(),
    }
}

/// Dial mount over the ticket address (IP and/or iroh relay). No WebRTC.
async fn connect_relay(
    ticket: MountTicket,
    token: [u8; SECRET_LEN],
) -> Result<ShareClient, JsValue> {
    ensure_reachable_addr(&ticket.addr)?;
    let key = SecretKey::generate();
    let endpoint = Endpoint::builder(presets::Minimal)
        .secret_key(key)
        .relay_mode(relay_mode(&ticket.lookups.relay))
        .bind()
        .await
        .map_err(|error| err("bind relay endpoint", &error))?;

    let connection = endpoint
        .connect(ticket.addr.clone(), MOUNT_ALPN)
        .await
        .map_err(|error| err("dial the mount ALPN over iroh relay/IP", &error))?;

    Ok(new_share_client(
        connection,
        token,
        ticket.author,
        "relay".to_owned(),
        None,
        None,
        None,
        endpoint,
    ))
}

/// Dial mount using **only** the ticket's relay URL(s) — no direct IP.
async fn connect_relay_only(
    ticket: MountTicket,
    token: [u8; SECRET_LEN],
) -> Result<ShareClient, JsValue> {
    let relays: Vec<TransportAddr> = ticket
        .addr
        .relay_urls()
        .cloned()
        .map(TransportAddr::Relay)
        .collect();
    if relays.is_empty() {
        return Err(JsValue::from_str(
            "ticket has no iroh relay URL — cannot force relay transport",
        ));
    }
    let relay_only = EndpointAddr::from_parts(ticket.addr.id, relays);
    let key = SecretKey::generate();
    // Browser endpoints have no IP transports (`clear_ip_transports` is
    // host-only); dialing relay-only plus path assertion is enough here.
    let endpoint = Endpoint::builder(presets::Minimal)
        .secret_key(key)
        .relay_mode(relay_mode(&ticket.lookups.relay))
        .bind()
        .await
        .map_err(|error| err("bind relay-only endpoint", &error))?;

    let connection = dial_with_retry(&endpoint, relay_only).await?;
    ensure_relay_selected(&connection).await?;

    Ok(new_share_client(
        connection,
        token,
        ticket.author,
        "relay".to_owned(),
        None,
        None,
        None,
        endpoint,
    ))
}

/// Retry dial for up to 90s (same policy as the native bench consumer).
async fn dial_with_retry(endpoint: &Endpoint, addr: EndpointAddr) -> Result<Connection, JsValue> {
    let deadline = now_ms() + 90_000.0;
    loop {
        match endpoint.connect(addr.clone(), MOUNT_ALPN).await {
            Ok(conn) => return Ok(conn),
            Err(error) if now_ms() < deadline => {
                web_sys::console::warn_1(&JsValue::from_str(&format!(
                    "bench dial failed; retrying: {error}"
                )));
                wait_ms(2_000).await;
            }
            Err(error) => {
                return Err(err(
                    "could not reach bench producer over iroh relay",
                    &error,
                ));
            }
        }
    }
}

/// Wait briefly for path selection, then require the selected path to be relay.
async fn ensure_relay_selected(conn: &Connection) -> Result<(), JsValue> {
    let deadline = now_ms() + 5_000.0;
    loop {
        if conn
            .paths()
            .iter()
            .find(|p| p.is_selected())
            .is_some_and(|p| p.is_relay())
        {
            return Ok(());
        }
        if now_ms() >= deadline {
            let summary: Vec<String> = conn
                .paths()
                .iter()
                .map(|p| {
                    let kind = if p.is_relay() {
                        "relay"
                    } else if p.is_ip() {
                        "ip"
                    } else {
                        "other"
                    };
                    if p.is_selected() {
                        format!("*{kind}")
                    } else {
                        kind.to_owned()
                    }
                })
                .collect();
            return Err(JsValue::from_str(&format!(
                "relay bench selected a non-relay path (paths={summary:?}); \
                 producer and consumer both need clear_ip_transports \
                 (restart producer with `bench --transport relay`)"
            )));
        }
        wait_ms(50).await;
    }
}

/// How long [`settled_path_label`] waits for the connection to pick a path.
const PATH_SETTLE_MS: f64 = 3_000.0;

/// Wait for path selection, then label the path that won.
///
/// Reading `paths()` the instant `connect` resolves reports a race, not a
/// result: a fresh connection has no selected path yet, and the mount dial runs
/// on the same endpoint that just spoke JSEP over the relay — so the producer's
/// relay addr is still a live candidate and can beat a data channel that is
/// only just coming up. Same settle-then-read shape as
/// [`ensure_relay_selected`], but it reports rather than judges: the caller
/// decides whether the answer is acceptable for its mode.
///
/// `None` means nothing was selected before the deadline.
async fn settled_path_label(conn: &Connection) -> Option<String> {
    let deadline = now_ms() + PATH_SETTLE_MS;
    loop {
        // Scoped so the `PathList` borrow of `conn` ends before the await.
        let selected = {
            let paths = conn.paths();
            paths
                .iter()
                .find(|path| path.is_selected())
                .map(|path| link::path_label(path.remote_addr()))
        };
        if selected.is_some() {
            return selected;
        }
        if now_ms() >= deadline {
            return None;
        }
        wait_ms(50).await;
    }
}

enum WatchEnd {
    /// Clean stream end before any frame — producer does not do live watch.
    Unsupported,
    /// Mid-stream error; retry on the same connection if it is still open.
    Retryable,
}

/// One OP_WATCH attempt: open a bi-stream, send the request, apply frames.
async fn follow_watch(
    conn: &Connection,
    token: &[u8; SECRET_LEN],
    on_manifest: &js_sys::Function,
) -> WatchEnd {
    let Ok((mut send, mut recv)) = conn.open_bi().await else {
        return WatchEnd::Retryable;
    };
    if send
        .write_all(&framing::encode_watch_request(token))
        .await
        .is_err()
    {
        return WatchEnd::Retryable;
    }
    if send.finish().is_err() {
        return WatchEnd::Retryable;
    }

    let mut manifest = MountManifest::default();
    let mut saw_frame = false;
    loop {
        let Ok(len) = read_header(&mut recv, MAX_MANIFEST_BYTES).await else {
            return if saw_frame {
                WatchEnd::Retryable
            } else {
                WatchEnd::Unsupported
            };
        };
        let mut body = vec![0u8; len as usize];
        if recv.read_exact(&mut body).await.is_err() {
            return WatchEnd::Retryable;
        }
        let Some((kind, payload)) = body.split_first() else {
            return WatchEnd::Retryable;
        };
        let applied = match *kind {
            framing::WATCH_FRAME_MANIFEST => {
                MountManifest::decode(payload).map(|fresh| manifest = fresh)
            }
            framing::WATCH_FRAME_DELTA => {
                ManifestDelta::decode(payload).map(|delta| manifest.apply(&delta))
            }
            _ => return WatchEnd::Retryable,
        };
        if applied.is_err() {
            return WatchEnd::Retryable;
        }
        saw_frame = true;
        let Ok(value) = serde_wasm(&manifest) else {
            return WatchEnd::Retryable;
        };
        if on_manifest.call1(&JsValue::NULL, &value).is_err() {
            web_sys::console::warn_1(&JsValue::from_str(
                "[share] watch callback threw; continuing subscription",
            ));
            continue;
        }
    }
}

/// Whether `only` selects `rel_path`. An empty filter takes everything.
///
/// A prefix match only at a path boundary, so `--only docs` cannot quietly take
/// `docsbackup/` too. Mirrors `agent_share`'s native `wanted`; the two must
/// agree or the same request would fetch different sets in a browser and a
/// terminal.
fn wanted(only: &[String], rel_path: &str) -> bool {
    only.is_empty()
        || only
            .iter()
            .any(|want| rel_path == want || rel_path.starts_with(&format!("{want}/")))
}

/// The store's name for a manifest entry.
///
/// Size and mtime come from the manifest rather than from anything local,
/// because they are what the *origin* says this version is. That is the
/// comparison the store's version gate makes on every read.
pub(crate) fn file_id(entry: &agent_share_proto::manifest::FileEntry) -> FileId {
    FileId {
        key: entry.rel_path.clone(),
        size: entry.size,
        mtime: entry.mtime,
    }
}

async fn wait_ms(millis: i32) {
    // `setTimeout` off the global rather than the `Window`: identical in a
    // page, and it keeps this future resolvable in the node test runner,
    // where there is no `Window` and a window-bound timer would hang every
    // future built on this one.
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        use wasm_bindgen::JsCast as _;
        let global = js_sys::global();
        if let Ok(set_timeout) = js_sys::Reflect::get(&global, &JsValue::from_str("setTimeout")) {
            let set_timeout: js_sys::Function = set_timeout.unchecked_into();
            let _ = set_timeout.call2(&global, &resolve, &JsValue::from(millis));
        }
    });
    let _ = JsFuture::from(promise).await;
}

/// Signal + WebRTC mount dial; optionally fall back to iroh relay/IP.
async fn connect_webrtc(
    ticket: MountTicket,
    token: [u8; SECRET_LEN],
    allow_relay_fallback: bool,
) -> Result<ShareClient, JsValue> {
    let producer = ticket.addr.id;
    // Copied out before the ticket moves into the relay fallback below.
    let author = ticket.author;
    ensure_reachable_addr(&ticket.addr)?;

    // **Two** endpoints on one key — and the split is what puts mount bytes on
    // the data channel at all.
    //
    // One endpoint cannot do both jobs. The JSEP exchange rides the relay, so
    // by the time the mount is dialled the endpoint's address book holds a warm
    // relay path for this producer. iroh merges a dial's address into that book
    // and only fans a connect's Initial out while the remote has no selected
    // path, so the relay answers first and becomes the connection's *only*
    // path — the WebRTC path is never opened, and no `path_selector` can pick a
    // path that does not exist. Measured, not assumed: with one endpoint the
    // mount connection reports `paths=["*relay"]`, with the split it reports
    // `paths=["*webrtc"]` (see `the_mount_selects_webrtc_over_a_warm_relay_path`
    // in `crates/agent-share/tests/webrtc_mount.rs`).
    //
    // Both endpoints bind the *same* token key, which is what makes this
    // different from the old two-endpoint shape this replaced. That one minted
    // two keys, so a producer counted the tab twice and the mount session lived
    // in a hub the mesh counter never read. One key means one endpoint id: the
    // producer attaches the session under the id the signal connection came
    // from, and the mount dial arrives under the same one.
    //
    // Only the signal endpoint may hold the relay. Two same-key endpoints both
    // registering with one relay fight over the registration and ICE never
    // completes — measured, the data channel simply times out.
    let key = SecretKey::generate();
    let local = key.public();

    // The mesh gossips over the relay, so it rides the signal endpoint and gets
    // its own hub for its own direct sessions. The mount's hub is separate;
    // `peers_direct` unions the two so the count stays honest.
    let mesh_hub = BrowserHubTransport::new(local);
    let mesh_handle = WebRtcHandle::new(Arc::clone(&mesh_hub));
    let signal_bind = async {
        Endpoint::builder(presets::Minimal)
            .secret_key(key.clone())
            .relay_mode(relay_mode(&ticket.lookups.relay))
            .add_custom_transport(mesh_handle.transport())
            .path_selector(mesh_handle.path_selector())
            .bind()
            .await
            .map_err(|error| err("bind signal endpoint", &error))
    };

    let hub = BrowserHubTransport::new(local);
    let handle = WebRtcHandle::new(Arc::clone(&hub));
    let mount_bind = async {
        Endpoint::builder(presets::Minimal)
            .secret_key(key.clone())
            // No relay, deliberately: this endpoint's whole purpose is to have
            // no path to lose the mount dial to.
            .relay_mode(RelayMode::Disabled)
            .add_custom_transport(handle.transport())
            .path_selector(handle.path_selector())
            .bind()
            .await
            .map_err(|error| err("bind mount endpoint", &error))
    };
    // Nothing links the two binds but the shared key, so they run together.
    let (signal_endpoint, endpoint) = futures::future::try_join(signal_bind, mount_bind).await?;

    // JSEP on the signal endpoint, attached into the *mount* endpoint's hub.
    // Decoupling those two is the point: the producer keys the session by the
    // id the signal connection came from, which is the same id either way.
    let session = match negotiate(&signal_endpoint, ticket.addr.clone(), local, &hub).await {
        Ok(session) => session,
        Err(error) if allow_relay_fallback => {
            let reason = format!("WebRTC signal/ICE failed: {}", describe(&error));
            web_sys::console::warn_1(&JsValue::from_str(&format!(
                "[agent-share] {reason}; falling back to iroh relay/IP"
            )));
            endpoint.close().await;
            // The fallback needs a relay, so it runs on the signal endpoint.
            return finish_relay_fallback(signal_endpoint, ticket, token, reason).await;
        }
        Err(error) => {
            endpoint.close().await;
            signal_endpoint.close().await;
            return Err(error);
        }
    };

    // The mount endpoint has no relay and (in a tab) no IP, so this address is
    // the only one it can reach the producer on.
    let webrtc_only =
        EndpointAddr::from_parts(producer, [TransportAddr::Custom(custom_addr(producer))]);
    match endpoint.connect(webrtc_only, MOUNT_ALPN).await {
        Ok(connection) => {
            // Pinned webrtc mode settles inline: it exists to *prove* the
            // transport for tests and bench, so it must not resolve before
            // the selected path is known. Dynamic mode defers the settle to
            // the background join task — on a relay-free, IP-free endpoint
            // there is nothing to settle on but WebRTC, so `"webrtc"` is a
            // provisional label rather than the old unconditional guess, and
            // the 3 s settle wait comes off the connect path.
            //
            // The *selected* path, not "is a WebRTC path present". Scanning
            // every path with `any()` answered a different question than the
            // one that matters — a connection can hold a WebRTC path it does
            // not send on — so it could report `webrtc` while the relay
            // carried the bytes, and the reverse.
            let data_path = if allow_relay_fallback {
                "webrtc".to_owned()
            } else {
                let selected = settled_path_label(&connection).await;
                if selected.as_deref() != Some("webrtc") {
                    let observed = path_labels(&connection);
                    endpoint.close().await;
                    signal_endpoint.close().await;
                    return Err(JsValue::from_str(&format!(
                        "mount connected but selected {} rather than WebRTC (paths={observed:?}), \
                         and webrtc mode forbids a fallback",
                        selected.as_deref().unwrap_or("no path"),
                    )));
                }
                "webrtc".to_owned()
            };
            // Captured before the endpoint moves into the mesh below.
            let rendezvous_relays: Vec<String> = signal_endpoint
                .addr()
                .relay_urls()
                .map(|url| url.to_string())
                .collect();
            let mut client = new_share_client(
                connection,
                token,
                author,
                data_path,
                Some(hub),
                Some(session),
                // The mesh rides the *signal* endpoint, the one with a relay to
                // gossip over, with its own hub.
                Some(MeshEndpoint {
                    endpoint: signal_endpoint,
                    webrtc: mesh_handle,
                }),
                endpoint,
            );
            client.rendezvous_relays = rendezvous_relays;
            client.settle_pending = allow_relay_fallback;
            Ok(client)
        }
        Err(error) if allow_relay_fallback => {
            let reason = format!("WebRTC mount dial failed: {error}");
            web_sys::console::warn_1(&JsValue::from_str(&format!(
                "[agent-share] {reason}; falling back to iroh relay/IP"
            )));
            endpoint.close().await;
            finish_relay_fallback(signal_endpoint, ticket, token, reason).await
        }
        Err(error) => {
            endpoint.close().await;
            signal_endpoint.close().await;
            Err(err("dial the mount ALPN over WebRTC", &error))
        }
    }
}

async fn finish_relay_fallback(
    endpoint: Endpoint,
    ticket: MountTicket,
    token: [u8; SECRET_LEN],
    reason: String,
) -> Result<ShareClient, JsValue> {
    let connection = endpoint
        .connect(ticket.addr.clone(), MOUNT_ALPN)
        .await
        .map_err(|error| err("dial the mount ALPN over iroh relay/IP (fallback)", &error))?;
    let client = new_share_client(
        connection,
        token,
        ticket.author,
        "relay".to_owned(),
        None,
        None,
        None,
        endpoint,
    );
    *client.fallback_reason.borrow_mut() = Some(reason);
    Ok(client)
}

/// A `JsValue` error as one line of prose, without the `JsValue("…")` wrapper
/// `{:?}` puts around a string.
fn describe(error: &JsValue) -> String {
    error.as_string().unwrap_or_else(|| format!("{error:?}"))
}

fn ensure_reachable_addr(addr: &EndpointAddr) -> Result<(), JsValue> {
    if addr.relay_urls().next().is_none() && addr.ip_addrs().next().is_none() {
        return Err(JsValue::from_str(
            "ticket has no relay or IP address — the producer was not reachable when the ticket was minted",
        ));
    }
    Ok(())
}

/// Swap one JSEP envelope each way over the signal ALPN, then attach into `hub`.
async fn negotiate(
    endpoint: &Endpoint,
    producer: EndpointAddr,
    local: fofoca::protocol::iroh_base::EndpointId,
    hub: &BrowserHubTransport,
) -> Result<BrowserSession, JsValue> {
    let producer_id = producer.id;
    // Today the hub is built a few lines above this call and is provably empty,
    // so this cannot fire. It is here because the invariant is "the hub is
    // newborn", and the day someone reuses a hub across dials — which is the
    // natural next refactor — a second negotiation for a peer we already reach
    // would burn a full ICE budget and then be refused at attach.
    if hub.has_session(&producer_id) {
        return Ok(BrowserSession {
            remote: producer_id,
        });
    }
    // The relay dial and the offer are independent — the offer needs only
    // `local` and `ice` — so the dial's latency hides inside ICE gathering.
    // If the dial loses the race with an error, dropping the offer future
    // closes its in-flight RTCPeerConnection (fofoca's pending offer arms a
    // close-on-drop guard), so nothing leaks.
    let dial = async {
        let conn = endpoint
            .connect(producer, WEBRTC_SIGNAL_ALPN)
            .await
            .map_err(|error| err("dial the signal ALPN", &error))?;
        let streams = conn
            .open_bi()
            .await
            .map_err(|error| err("open signal stream", &error))?;
        Ok((conn, streams))
    };
    // STUN only — TURN is refused by policy; the iroh relay is this
    // project's relay, and running a second one at the ICE layer would mean
    // operating two systems for one job.
    let ice = IceServers::default();
    let build_offer = async {
        browser_offer(local, &ice)
            .await
            .map_err(|error| js_stage("build offer", error))
    };
    let ((conn, (mut send, mut recv)), (pending, offer)) =
        futures::future::try_join(dial, build_offer).await?;
    let encoded = serde_json::to_vec(&offer).map_err(|error| err("encode offer", &error))?;
    send.write_all(&encoded)
        .await
        .map_err(|error| err("send offer", &error))?;
    send.finish().map_err(|error| err("finish", &error))?;

    let raw = recv
        .read_to_end(MAX_ENVELOPE_BYTES)
        .await
        .map_err(|error| {
            err(
                "read answer (producer never answered — is it still sharing?)",
                &error,
            )
        })?;
    let answer: SignalEnvelope =
        serde_json::from_slice(&raw).map_err(|error| err("parse answer", &error))?;
    // An explicit refusal, which a producer sends when it already holds a
    // session with us. Named here rather than left to `complete`, which reports
    // it through `claimed_endpoint()` as "remote signaling error" from inside a
    // stage called "build offer" — true but useless.
    if let SignalEnvelope::Error { reason, .. } = &answer {
        if hub.has_session(&producer_id) {
            return Ok(BrowserSession {
                remote: producer_id,
            });
        }
        return Err(JsValue::from_str(&format!(
            "producer refused WebRTC signalling: {reason}"
        )));
    }
    let session = match pending.complete(hub, &answer).await {
        Ok(session) => session,
        Err(error) => {
            log_signal_sdps("consumer", &offer, &answer);
            return Err(js_stage("complete WebRTC offer", error));
        }
    };
    // Hub is keyed by the answer's claimed id; the mount dial uses the ticket
    // id. A mismatch would blackhole transmits with no useful error.
    if session.remote != producer_id {
        return Err(JsValue::from_str(&format!(
            "answer endpoint id {} does not match ticket producer {}",
            session.remote, producer_id
        )));
    }

    conn.close(0u32.into(), b"jsep done");
    Ok(session)
}

fn js_stage(context: &str, error: JsValue) -> JsValue {
    JsValue::from_str(&format!("{context}: {error:?}"))
}

/// Read the `status(1) ‖ len(u32)` prefix every response carries.
/// Read just the length that follows a status byte the caller already took.
///
/// Split from [`read_header`] because the chunk ops read their status first:
/// a non-`Ok` status there is "I cannot answer for that", which is ordinary
/// traffic rather than a protocol failure, and folding the two together would
/// turn every polite refusal into an error.
async fn read_len(
    recv: &mut fofoca::iroh::endpoint::RecvStream,
    cap: u32,
) -> Result<u32, JsValue> {
    let mut raw = [0u8; 4];
    recv.read_exact(&mut raw)
        .await
        .map_err(|error| err("read response length", &error))?;
    let len = u32::from_le_bytes(raw);
    if len > cap {
        return Err(JsValue::from_str(&format!(
            "a peer answered with {len} bytes, over the {cap}-byte cap"
        )));
    }
    Ok(len)
}

async fn read_header(
    recv: &mut fofoca::iroh::endpoint::RecvStream,
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
///
/// The length of this ladder does not set how many relay `WebSocket`s a tab
/// opens, so trimming it does not quiet the red `WebSocket … failed` lines in
/// the console. On wasm, iroh measures a rung with a `fetch` of `/ping`, and it
/// opens a socket only for the home relay and for each rung a *peer* advertises.
/// That is why [`seeder_relays`] naming all five rungs costs five sockets, four
/// of them guesses that iroh's inactive-relay reap kills a minute later. A
/// socket that closes before it opens always logs an error, and no browser API
/// can silence it. Removing the noise needs a cap on speculative relay paths in
/// iroh, not a change here.
///
/// Takes the choice rather than the ticket so a *producing* tab, which binds
/// before it has a ticket to hand, homes on the ladder its ticket will go on to
/// advertise.
pub(crate) fn relay_mode(choice: &agent_share_proto::lookup::RelayChoice) -> RelayMode {
    use agent_share_proto::lookup::RelayChoice;
    match choice {
        RelayChoice::Disabled => RelayMode::Disabled,
        // Our relay first, n0's as fallback — the same rungs the mesh gossips
        // over, taken from the one list rather than a second copy. A ticket
        // that says "pinned" carries no URLs, so the producer and this tab
        // resolve the name independently; if the two lists ever disagreed the
        // pair would home on different relays and simply never meet.
        RelayChoice::Pinned => RelayMode::custom(pinned_ladder()),
        RelayChoice::Custom(ladder) => RelayMode::custom(ladder.iter().cloned()),
    }
}

fn err(context: &str, error: &impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&format!("{context}: {error}"))
}

/// Prefix every credential failure carries, so the page can tell "wrong
/// password, ask again" from "this share is broken" without parsing prose.
///
/// A string rather than a typed error because everything crossing
/// `wasm_bindgen` is a `JsValue`, and a bare string is the one shape TypeScript
/// can read without a wrapper on both sides.
pub const UNAUTHORIZED_PREFIX: &str = "unauthorized:";

/// Build the error the page re-prompts on.
fn unauthorized(detail: &str) -> JsValue {
    JsValue::from_str(&format!("{UNAUTHORIZED_PREFIX} {detail}"))
}

/// Whether a connection's close is the producer refusing the credential.
///
/// Matched on the application close code the producer chose, not on the reason
/// string — the string is a human label and not wire format.
fn unauthorized_close(conn: &Connection) -> bool {
    matches!(
        conn.close_reason(),
        Some(fofoca::iroh::endpoint::ConnectionError::ApplicationClosed(close))
            if u64::from(close.error_code) == u64::from(framing::CLOSE_UNAUTHORIZED)
    )
}

/// What this client will present, refusing the mismatches before the dial.
///
/// The browser twin of the CLI's `redeem_auth`. Both directions are errors: a
/// protected ticket with no password cannot succeed, and a password offered to
/// an unprotected ticket almost always means the wrong ticket, which would
/// otherwise open the wrong share and look like it worked.
fn redeem_auth(ticket: &MountTicket, password: Option<&str>) -> Result<ShareAuth, JsValue> {
    match (ticket.password_protected(), password) {
        (true, None) => Err(unauthorized("this share needs a password")),
        (false, Some(_)) => Err(JsValue::from_str(
            "this ticket is not password-protected — check you have the right link",
        )),
        (_, password) => Ok(ShareAuth::new(&ticket.secret, password)),
    }
}

/// Explain a failure to open a stream in terms of what actually broke.
///
/// `open_bi()` has no timeout of its own: it parks until stream credit arrives
/// and the *only* way it returns an error is a connection-level failure. So
/// reporting it as "open read stream: timed out" blames stream setup for the
/// connection's death, which is how one such failure cost an afternoon.
///
/// `TimedOut` is the connection's idle timeout, and in a tab the way a share
/// reaches it is being backgrounded: Safari throttles timers to 13–20 s
/// intervals after roughly ten seconds hidden (measured), which is longer than
/// QUIC's keep-alive interval, so the connection goes quiet and expires. The
/// message says so, because "timed out" alone sends the reader looking at the
/// network.
fn stream_open_failed(what: &str, error: &fofoca::iroh::endpoint::ConnectionError) -> JsValue {
    if matches!(error, fofoca::iroh::endpoint::ConnectionError::TimedOut) {
        return JsValue::from_str(&format!(
            "{what}: the connection to the producer expired while idle. \
             A backgrounded tab throttles timers below the keep-alive interval, \
             which is enough to lose it. Reload the page to reconnect."
        ));
    }
    err(&format!("{what}: connection to the producer lost"), error)
}

fn serde_wasm<T: serde::Serialize>(value: &T) -> Result<JsValue, JsValue> {
    let json = serde_json::to_string(value).map_err(|error| err("serialize", &error))?;
    js_sys::JSON::parse(&json)
}

/// The `Pinned` ladder, from `agent-habilis-mesh` — see [`relay_mode`].
///
/// The engine parses the list, not us: `relay_ladder` is `LazyLock`-cached and
/// its `RelayUrl`s are `Arc`-backed, so this costs a clone rather than five URL
/// parses per endpoint. The CLI reaches the same list through the same call.
fn pinned_ladder() -> Vec<fofoca::iroh::RelayUrl> {
    fofoca::net::relay_ladder(&fofoca::protocol::RelayChoice::Pinned)
}

#[cfg(test)]
mod tests {
    use super::{
        CHANNEL_DISCONNECT_GRACE_MS, PeerCard, ProbeChunkSource, RaceOutcome, SwarmView,
        drain_with_stall_deadline, first_success, pinned_ladder, relay_mode, swarm_rows,
    };
    use wasm_bindgen::{JsCast, JsValue};
    use wasm_bindgen_futures::JsFuture;

    // These run on wasm32, the only target this crate builds for — see the
    // dev-dependency note in `Cargo.toml`. Renaming the attribute keeps the
    // tests written as ordinary `#[test]` functions.
    use wasm_bindgen_test::wasm_bindgen_test as test;

    /// A producing tab homes on the ladder its ticket goes on to advertise.
    ///
    /// It used to bind with iroh's `default_relay_mode()`, which names n0's four
    /// relays and not rung 0 — ours. So a tab served a "pinned" ticket from a
    /// relay no reader of that ticket would look on, and two tabs sharing to
    /// each other were guaranteed to start apart. Compared as sets: `RelayMap`
    /// is a `BTreeMap`, so rung order does not survive, and nothing downstream
    /// wants it to — iroh picks its home relay by measured latency.
    #[test]
    fn a_pinned_endpoint_is_offered_our_relay() {
        let ladder = pinned_ladder();
        let ours = ladder.first().expect("the pinned ladder has rungs").clone();
        assert_eq!(ours.host_str(), Some("relay.agent-habilis.com"));

        let choice = agent_share_proto::lookup::LookupOpts::public_preset().relay;
        let mut offered = relay_mode(&choice).relay_map().urls::<Vec<_>>();
        let mut expected = ladder;
        offered.sort();
        expected.sort();
        assert_eq!(offered, expected);
        assert!(offered.contains(&ours));
    }

    /// A loopback ticket reaches no relay at all.
    #[test]
    fn a_disabled_choice_names_no_relay() {
        let offered = relay_mode(&agent_share_proto::lookup::RelayChoice::Disabled)
            .relay_map()
            .urls::<Vec<fofoca::iroh::RelayUrl>>();
        assert!(offered.is_empty());
    }

    /// `wait_ms` needs a `Window`; the node test runner has none, but
    /// `setTimeout` lives on the global in both worlds.
    async fn sleep(ms: i32) {
        let promise = js_sys::Promise::new(&mut |resolve, _reject| {
            let global = js_sys::global();
            let set_timeout = js_sys::Reflect::get(&global, &JsValue::from_str("setTimeout"))
                .expect("setTimeout exists on the test global");
            let set_timeout: js_sys::Function = set_timeout.unchecked_into();
            let _ = set_timeout.call2(&global, &resolve, &JsValue::from(ms));
        });
        let _ = JsFuture::from(promise).await;
    }

    /// Yields `chunk_len` bytes `chunks` times, sleeping `gap_ms` before
    /// each; then hangs forever or ends the stream, per `then_hang`.
    struct Scripted {
        chunks: usize,
        chunk_len: usize,
        gap_ms: i32,
        then_hang: bool,
    }

    impl ProbeChunkSource for Scripted {
        async fn next_chunk(&mut self) -> Result<Option<usize>, JsValue> {
            if self.chunks == 0 {
                if self.then_hang {
                    std::future::pending::<()>().await;
                }
                return Ok(None);
            }
            sleep(self.gap_ms).await;
            self.chunks -= 1;
            Ok(Some(self.chunk_len))
        }
    }

    /// A slow link is not a stalled link. Eight chunks arriving every 40 ms
    /// take 320 ms — more than three full 100 ms stall windows — and must
    /// pass, because every chunk re-arms the clock. Under the old whole-read
    /// deadline this exact stream failed: 256 KiB raced against one fixed
    /// timer is a throughput floor, and it discarded slow-but-healthy
    /// seeders that may have held the only copy of a share.
    #[test]
    async fn slow_but_moving_probe_passes() {
        let mut source = Scripted {
            chunks: 8,
            chunk_len: 32 * 1024,
            gap_ms: 40,
            then_hang: false,
        };
        let outcome = drain_with_stall_deadline(&mut source, 256 * 1024, || sleep(100)).await;
        assert!(
            outcome.is_ok(),
            "a moving stream must outlive any number of stall windows: {outcome:?}"
        );
    }

    /// The defect the probe hunts: first bytes land, then nothing, forever.
    #[test]
    async fn frozen_probe_fails_within_one_window() {
        let mut source = Scripted {
            chunks: 1,
            chunk_len: 39,
            gap_ms: 0,
            then_hang: true,
        };
        let outcome = drain_with_stall_deadline(&mut source, 256 * 1024, || sleep(100)).await;
        assert!(
            outcome.is_err(),
            "a stream frozen after its first bytes must stall out"
        );
    }

    /// An early end is a failure, not a pass — the probe asked for more.
    #[test]
    async fn short_probe_body_fails() {
        let mut source = Scripted {
            chunks: 2,
            chunk_len: 1024,
            gap_ms: 0,
            then_hang: false,
        };
        let outcome = drain_with_stall_deadline(&mut source, 256 * 1024, || sleep(100)).await;
        assert!(
            outcome.is_err(),
            "a stream that ends early must not pass the probe"
        );
    }

    fn entry(rel_path: &str, size: u64) -> agent_share_proto::manifest::FileEntry {
        agent_share_proto::manifest::FileEntry {
            rel_path: rel_path.to_owned(),
            size,
            mode: 0o644,
            mtime: 0,
        }
    }

    /// A 1 KiB README ahead of a 1 MiB payload used to green-light the
    /// channel: the seeder clamps a read to file size, the first live file
    /// was 1 KiB, and any non-empty answer passed — so the "bulk" probe
    /// moved 1 KiB and the first real 256 KiB frame froze. The probe must
    /// pick the largest file and ask for the full window it can serve.
    #[test]
    fn probe_reads_the_largest_file_not_the_first() {
        let files = [entry("README", 1024), entry("payload.bin", 1024 * 1024)];
        assert_eq!(super::probe_target(&files), Some((1, 256 * 1024)));
    }

    /// When every file is small the request shrinks to match — and
    /// `probe_read` then demands exactly that much back, so a short answer
    /// can never masquerade as a pass.
    #[test]
    fn probe_request_clamps_to_the_largest_file() {
        let files = [entry("", 0), entry("small", 4096)];
        assert_eq!(super::probe_target(&files), Some((1, 4096)));
    }

    /// All tombstones or empty files: nothing to prove, and the caller says
    /// so in the log instead of skipping silently.
    #[test]
    fn no_probe_target_on_a_manifest_with_no_bytes() {
        let files = [entry("", 0), entry("emptied", 0)];
        assert_eq!(super::probe_target(&files), None);
    }

    type Attempt = std::pin::Pin<Box<dyn std::future::Future<Output = Result<u32, String>>>>;

    /// The ghost-card scenario, and the reason the candidate loop races:
    /// serially, a dead candidate ahead of the live seeder consumed its
    /// whole budget before the live one was even dialled — twenty ghosts
    /// meant ten minutes of "connecting" against a share that was up. Raced,
    /// a hung attempt cannot block a winner.
    #[test]
    async fn race_winner_beats_a_hanging_candidate() {
        let attempts: Vec<Attempt> = vec![
            Box::pin(async {
                std::future::pending::<()>().await;
                Err("ghost".to_owned())
            }),
            Box::pin(async {
                sleep(50).await;
                Ok(7)
            }),
        ];
        let mut refusals = Vec::new();
        let outcome = first_success(attempts, Box::pin(sleep(2_000)), &mut refusals).await;
        assert!(
            matches!(outcome, RaceOutcome::Winner(7)),
            "a hung candidate must not block a live one"
        );
        assert!(refusals.is_empty(), "nobody refused: {refusals:?}");
    }

    /// A refusal removes one runner and the race keeps going.
    #[test]
    async fn race_survives_refusals() {
        let attempts: Vec<Attempt> = vec![
            Box::pin(async { Err("dead on arrival".to_owned()) }),
            Box::pin(async {
                sleep(50).await;
                Ok(1)
            }),
        ];
        let mut refusals = Vec::new();
        let outcome = first_success(attempts, Box::pin(sleep(2_000)), &mut refusals).await;
        assert!(matches!(outcome, RaceOutcome::Winner(1)));
        assert_eq!(refusals, vec!["dead on arrival".to_owned()]);
    }

    /// Nothing but ghosts: the deadline hands control back to the caller's
    /// retry loop instead of hanging the attempt forever.
    #[test]
    async fn race_deadline_bounds_an_all_ghost_roster() {
        let attempts: Vec<Attempt> = vec![Box::pin(async {
            std::future::pending::<()>().await;
            Err("ghost".to_owned())
        })];
        let mut refusals = Vec::new();
        let outcome = first_success(attempts, Box::pin(sleep(100)), &mut refusals).await;
        assert!(matches!(outcome, RaceOutcome::DeadlineExpired));
    }

    /// Every candidate refusing is its own terminal state, with all the
    /// refusals collected for the error message.
    #[test]
    async fn race_reports_when_everyone_refuses() {
        let attempts: Vec<Attempt> = vec![
            Box::pin(async { Err("one".to_owned()) }),
            Box::pin(async {
                sleep(20).await;
                Err("two".to_owned())
            }),
        ];
        let mut refusals = Vec::new();
        let outcome = first_success(attempts, Box::pin(sleep(2_000)), &mut refusals).await;
        assert!(matches!(outcome, RaceOutcome::AllFailed));
        assert_eq!(refusals.len(), 2);
    }

    /// The states nothing ever observed: `failed` is terminal and must kill
    /// immediately — before this watcher existed, a failed channel sat
    /// behind a connected-looking tab until QUIC's idle timeout gave up.
    #[test]
    fn failed_ice_kills_without_grace() {
        use web_sys::RtcPeerConnectionState as State;
        assert!(matches!(
            super::judge_channel(State::Failed, None),
            super::ChannelVerdict::Kill(_)
        ));
        assert!(matches!(
            super::judge_channel(State::Closed, None),
            super::ChannelVerdict::Kill(_)
        ));
    }

    /// `disconnected` is the one recoverable state: inside the grace window
    /// it only waits, past it the channel is declared dead.
    #[test]
    fn disconnected_gets_grace_then_dies() {
        use super::{CHANNEL_DISCONNECT_GRACE_MS, ChannelVerdict, judge_channel};
        use web_sys::RtcPeerConnectionState as State;
        assert_eq!(
            judge_channel(State::Disconnected, None),
            ChannelVerdict::Wait
        );
        assert_eq!(
            judge_channel(State::Disconnected, Some(CHANNEL_DISCONNECT_GRACE_MS / 2.0)),
            ChannelVerdict::Wait
        );
        assert!(matches!(
            judge_channel(State::Disconnected, Some(CHANNEL_DISCONNECT_GRACE_MS)),
            ChannelVerdict::Kill(_)
        ));
    }

    /// A recovery mid-grace clears the window: connected reports healthy no
    /// matter how long the previous disconnect stood.
    #[test]
    fn recovery_resets_the_grace_window() {
        use web_sys::RtcPeerConnectionState as State;
        assert_eq!(
            super::judge_channel(State::Connected, Some(CHANNEL_DISCONNECT_GRACE_MS * 2.0)),
            super::ChannelVerdict::Healthy
        );
    }

    /// One offer at entry, one more past the halfway mark, never a third —
    /// and the halfway mark tracks the wait, so cutting the wait keeps the
    /// re-offer schedule coherent by construction.
    #[test]
    fn reoffer_schedule_is_entry_then_halfway() {
        assert!(super::reoffer_due(0, 0.0, 15_000.0));
        assert!(!super::reoffer_due(1, 7_000.0, 15_000.0));
        assert!(super::reoffer_due(1, 7_501.0, 15_000.0));
        assert!(!super::reoffer_due(2, 14_999.0, 15_000.0));
        assert!(super::reoffer_due(1, 3_001.0, 6_000.0));
    }

    type Dial = std::pin::Pin<Box<dyn std::future::Future<Output = Result<u32, JsValue>>>>;

    /// A waiting membership that a client is homed on must survive another
    /// connect's cleanup. The origin-win path used to close it regardless,
    /// which cut the retiring client's peers off mid-download — `Endpoint`
    /// clones share one socket — and stopped it serving before its
    /// replacement had re-armed. Reference counting also has to *release*:
    /// a client that lets go must leave the membership retirable again, or
    /// the cleanup this guards would never run at all.
    #[test]
    fn a_membership_a_client_lives_on_is_never_closed() {
        use std::rc::Rc;
        let peer = Rc::new(());
        assert!(
            super::safe_to_retire(Rc::strong_count(&peer)),
            "nothing but the registry holds it: this is the case cleanup exists for"
        );
        let adopted = Rc::clone(&peer);
        assert!(
            !super::safe_to_retire(Rc::strong_count(&peer)),
            "a client is homed on these endpoints; closing them cuts its peers off"
        );
        drop(adopted);
        assert!(
            super::safe_to_retire(Rc::strong_count(&peer)),
            "the client let go, so the membership is collectable again"
        );
    }

    /// Idle is not the same as garbage. A tab that navigated away from a
    /// share keeps serving it from a membership nobody is homed on, which is
    /// how a swarm keeps copies alive after the viewers are gone — so only a
    /// membership an origin win found to be a *duplicate* may be collected,
    /// and only once its last holder has let go.
    #[test]
    fn only_a_duplicate_membership_is_ever_collected() {
        use super::collect_duplicate_now;
        assert!(collect_duplicate_now(true, 1), "a duplicate nobody holds");
        assert!(
            !collect_duplicate_now(true, 2),
            "a duplicate is still off limits while a client is homed on it"
        );
        assert!(
            !collect_duplicate_now(false, 1),
            "an idle membership is this tab still seeding, not garbage"
        );
    }

    /// A cap this module chooses for itself has to clear the cold dial a
    /// live origin was measured needing, or a healthy producer is
    /// unreachable no matter how often the App retries — every attempt
    /// re-derives the same too-short cap from the same inputs. A former
    /// seeder used to get 8 s here for the rest of the tab's life.
    #[test]
    fn a_self_chosen_origin_cap_clears_an_honest_cold_dial() {
        assert!(
            super::default_origin_cap_ms() >= super::MEASURED_COLD_ORIGIN_DIAL_MS,
            "a self-chosen cap of {} ms cuts off a dial measured at {} ms",
            super::default_origin_cap_ms(),
            super::MEASURED_COLD_ORIGIN_DIAL_MS,
        );
    }

    /// A card is a claim; only a dial that landed is proof. The rule the
    /// concession runs on must ignore everything short of one, however long
    /// the dial has been running.
    #[test]
    fn only_an_answered_peer_concedes_the_origin() {
        use super::{ORIGIN_CONCEDE_FLOOR_MS as FLOOR, concede_due};
        assert!(!concede_due(false, FLOOR * 100.0, FLOOR));
        assert!(!concede_due(true, FLOOR / 2.0, FLOOR));
        assert!(concede_due(true, FLOOR, FLOOR));
    }

    /// The ghost-roster livelock this guard exists for. A share whose
    /// producer went away leaves cards behind forever — nothing deletes a
    /// departed peer's CRDT entry — so a lane that finds cards, dials them
    /// and reaches nobody leaves the proof where it started, and a returning
    /// producer gets its whole 6-20 s cold dial. Conceding on the card
    /// instead killed that dial at the 5 s floor, and the retry met the same
    /// ghosts.
    ///
    /// That the card path cannot raise the proof is the compiler's job, not
    /// this test's: [`MeshCanServe::peer_answered`] demands a `Connection`,
    /// which only a landed dial can produce. What is asserted here is the
    /// other half — an unraised proof leaves a slow origin alone however
    /// long the floor has passed.
    #[test]
    async fn ghost_cards_alone_never_concede_the_origin() {
        let can_serve = super::MeshCanServe::default();
        // An honest origin answering well past the floor, the way a cold
        // relay handshake does.
        let dial: Dial = Box::pin(async {
            sleep(300).await;
            Ok(42)
        });
        let outcome = super::capped_origin_dial(dial, 5_000, Some((can_serve, 50.0))).await;
        assert!(
            matches!(outcome, Ok(42)),
            "a roster of ghosts must not cut a live origin's dial short: {outcome:?}"
        );
    }

    /// The other half of the same rule: a peer that really answered does
    /// concede, so a lane that fails *after* connecting hands control back
    /// to the caller in a watcher tick instead of the cap's remainder.
    #[test]
    async fn origin_dial_concedes_once_cards_vouch() {
        let flag = super::MeshCanServe::already_answered();
        let dial: Dial = Box::pin(async {
            std::future::pending::<()>().await;
            unreachable!("the dial never resolves")
        });
        let outcome = super::capped_origin_dial(dial, 5_000, Some((flag, 0.0))).await;
        let message = outcome.expect_err("a vouched-for share must concede the dial");
        assert!(
            message.as_string().unwrap_or_default().contains("conceded"),
            "the error must name the concession: {message:?}"
        );
    }

    /// The floor guards a healthy share with a slow origin: a seeder that
    /// answers early cannot concede a dial that is about to win.
    #[test]
    async fn origin_dial_win_beats_an_early_card() {
        let flag = super::MeshCanServe::already_answered();
        let dial: Dial = Box::pin(async {
            sleep(50).await;
            Ok(9)
        });
        let outcome = super::capped_origin_dial(dial, 5_000, Some((flag, 2_000.0))).await;
        assert!(
            matches!(outcome, Ok(9)),
            "a dial resolving inside the floor must win"
        );
    }

    /// With nobody answering, the cap still governs, exactly as before the
    /// concession existed.
    #[test]
    async fn origin_dial_times_out_with_no_cards() {
        let flag = super::MeshCanServe::default();
        let dial: Dial = Box::pin(async {
            std::future::pending::<()>().await;
            unreachable!("the dial never resolves")
        });
        let outcome = super::capped_origin_dial(dial, 100, Some((flag, 0.0))).await;
        let message = outcome.expect_err("the cap must fire");
        assert!(
            message
                .as_string()
                .unwrap_or_default()
                .contains("timed out"),
            "the error must name the timeout: {message:?}"
        );
    }

    fn card(endpoint: &str, tree: Option<&str>) -> agent_share_proto::PeerCard {
        agent_share_proto::PeerCard {
            endpoint: endpoint.to_owned(),
            app: "agent-share".to_owned(),
            version: "0".to_owned(),
            runtime: "test".to_owned(),
            transport: "webrtc".to_owned(),
            client: "test".to_owned(),
            role: Some("consumer".to_owned()),
            tree: tree.map(str::to_owned),
            serving: tree.map(|_| "AA".to_owned()),
        }
    }

    /// The vote the card path picks a candidate with: most cards win, a
    /// card with no tree does not vote, and nobody vouching is `None`
    /// rather than a panic.
    #[test]
    fn the_majority_tree_is_the_one_most_cards_carry() {
        let roster = [
            card("a", Some("tree1")),
            card("b", Some("tree2")),
            card("c", Some("tree2")),
            card("d", None),
        ];
        assert_eq!(
            super::majority_tree(&roster)
                .as_ref()
                .map(super::MajorityTree::as_str),
            Some("tree2")
        );
        assert_eq!(super::majority_tree(&[card("a", None)]), None);
        assert_eq!(super::majority_tree(&[]), None);
    }

    /// The redial lane skips the vote to go fast, so the vote has to be able
    /// to catch up with it. A seeder frozen on the tree this tab happens to
    /// remember used to be adopted, re-recorded and re-adopted forever, with
    /// the mesh never getting a say.
    #[test]
    fn a_roster_on_another_tree_contradicts_what_the_redial_pinned() {
        use super::TreeVerdict;
        let moved_on = [card("b", Some("tree2")), card("c", Some("tree2"))];
        assert_eq!(
            super::judge_adopted_tree(&moved_on, "me", "tree1"),
            TreeVerdict::Disagrees
        );
        assert_eq!(
            super::judge_adopted_tree(&moved_on, "me", "tree2"),
            TreeVerdict::Agrees
        );
    }

    /// Silence is not disagreement. A roster that is empty, that holds only
    /// cards which cannot vouch, or that holds nothing but this tab's own
    /// card must leave the adoption alone — otherwise the first connect on a
    /// quiet mesh would throw away the record that made it fast.
    #[test]
    fn a_silent_roster_never_contradicts_anything() {
        use super::TreeVerdict;
        assert_eq!(
            super::judge_adopted_tree(&[], "me", "tree1"),
            TreeVerdict::Unknown
        );
        assert_eq!(
            super::judge_adopted_tree(&[card("b", None)], "me", "tree1"),
            TreeVerdict::Unknown
        );
        // This tab publishes its own card for the tree it just adopted;
        // counting it would let the adoption vouch for itself.
        assert_eq!(
            super::judge_adopted_tree(&[card("me", Some("tree1"))], "me", "tree1"),
            TreeVerdict::Unknown
        );
    }

    /// Two attempts for one share overlap all the time — minting is three
    /// endpoint binds and a mesh join, and a `/files` → `/info` → back
    /// navigation starts a second attempt inside the first. Both find the
    /// registry empty, and the loser's membership has to come back to its
    /// owner so it can be retired: a silently displaced `WaitingMesh` keeps
    /// its card on the roster and its two relay registrations for the life
    /// of the page, with nothing left holding a reference to say goodbye.
    #[test]
    fn a_losing_mint_gets_its_membership_back_to_retire() {
        let registry = std::cell::RefCell::new(std::collections::HashMap::new());
        assert_eq!(
            super::claim_waiting_mesh(&registry, "share", 1),
            None,
            "the first attempt claims the slot"
        );
        assert_eq!(
            super::claim_waiting_mesh(&registry, "share", 2),
            Some(1),
            "the second attempt is handed the winner, and keeps its own to retire"
        );
        assert_eq!(
            registry.borrow().get("share"),
            Some(&1),
            "the winner stays registered rather than being displaced"
        );
    }

    /// The derivation the lane deadlines are built from, exercised as a
    /// function rather than as the constants it happens to produce — those
    /// are held by compile-time assertions beside them. A budget has to
    /// leave room for the probe on top of reaching and verifying a peer,
    /// and a longer channel wait has to buy a longer budget, or a lane
    /// could be lengthened into being cut short again.
    #[test]
    fn a_vet_budget_grows_with_what_it_has_to_contain() {
        use super::{vet_budget_ms, vet_floor_ms};
        assert!(vet_budget_ms(6_000) > vet_floor_ms(6_000));
        assert!(vet_floor_ms(30_000) > vet_floor_ms(6_000));
        assert!(vet_budget_ms(30_000) > vet_budget_ms(6_000));
        // The floor is what a candidate needs before its probe begins, so
        // it can never be under the channel wait it contains.
        assert!(vet_floor_ms(30_000) >= 30_000);
    }

    /// A hidden tab's timers are clamped, and hiding the tab is what kills
    /// the connection, so the reconnect path runs throttled by definition.
    /// A budget shorter than one clamped tick used to give such a tab a
    /// single poll: the first wake was already past the deadline.
    #[test]
    fn a_throttled_tab_still_gets_its_chances() {
        use super::{BACKGROUND_TICK_MS, MIN_POLLS_BEFORE_GIVING_UP, out_of_time};
        let budget = 12_000.0;
        // Every wake in a background tab lands a whole tick later, so the
        // clock is blown from the very first one.
        for poll in 1..MIN_POLLS_BEFORE_GIVING_UP {
            let elapsed = BACKGROUND_TICK_MS * f64::from(poll);
            assert!(
                !out_of_time(elapsed, poll, budget),
                "poll {poll} at {elapsed} ms must still count as a chance"
            );
        }
        assert!(
            out_of_time(
                BACKGROUND_TICK_MS * f64::from(MIN_POLLS_BEFORE_GIVING_UP),
                MIN_POLLS_BEFORE_GIVING_UP,
                budget
            ),
            "the budget still has to end, once the chances are actually spent"
        );
        // A foreground tab is governed by its clock, exactly as before: the
        // poll count is spent long before the budget runs out.
        assert!(!out_of_time(budget / 2.0, 12, budget));
        assert!(out_of_time(budget + 1.0, 24, budget));
    }

    fn refusal(endpoint: &str, unreached: bool) -> super::Refusal {
        super::Refusal {
            endpoint: endpoint.to_owned(),
            reason: format!("{endpoint}: refused"),
            unreached,
        }
    }

    fn kept(list: &[super::KnownSeeder]) -> Vec<&str> {
        list.iter().map(|entry| entry.endpoint.as_str()).collect()
    }

    /// A peer that answered is alive, whatever went wrong next. The redial
    /// lane vets against the tree this tab recorded, so a seeder that
    /// correctly *followed* the origin to a new tree fails its vet — and
    /// deleting it for that is exactly backwards: the roster keeps the
    /// frozen peer and throws away the current one.
    #[test]
    fn a_seeder_that_answered_is_never_forgotten() {
        let list = vec![seeder("moved-on", 0.0), seeder("corpse", 0.0)];
        let refusals = [refusal("moved-on", false), refusal("corpse", true)];
        assert!(
            kept(&super::roster_after_failed_lane(list, &refusals)).contains(&"moved-on"),
            "a peer that served us a manifest is not a corpse"
        );
    }

    /// The lane's 10 s deadline cannot contain its own pipeline: a 6 s JSEP
    /// wait, a manifest fetch, then 10 s bulk-probe windows. So a live
    /// seeder whose channel formed at 2 s is routinely still mid-probe when
    /// the deadline fires, and it never refused anything — the lane simply
    /// ran out of time. Deleting it on that basis deletes the working peer.
    #[test]
    fn a_seeder_still_dialling_at_the_deadline_is_never_forgotten() {
        let list = vec![seeder("slow-but-live", 0.0), seeder("corpse", 0.0)];
        // Only the corpse got as far as refusing before the deadline.
        let refusals = [refusal("corpse", true)];
        assert!(
            kept(&super::roster_after_failed_lane(list, &refusals)).contains(&"slow-but-live"),
            "an attempt cut off mid-flight is not evidence about its peer"
        );
    }

    /// Reaching nobody at all says more about this tab's link than about
    /// everyone else's liveness. A laptop waking with its interfaces still
    /// down fails every dial in milliseconds, and wiping the roster there
    /// costs the fast lane in the exact scenario it was built for.
    #[test]
    fn a_lane_that_reached_nobody_keeps_everybody() {
        let list = vec![seeder("a", 0.0), seeder("b", 0.0)];
        let refusals = [refusal("a", true), refusal("b", true)];
        assert_eq!(
            kept(&super::roster_after_failed_lane(list, &refusals)),
            vec!["a", "b"],
            "one bad moment on our own link must not empty the roster"
        );
    }

    /// The other half: a record still has to be spendable, or a roster of
    /// corpses would cost the redial lane its whole budget on every single
    /// reconnect until the 24 h TTL finally expired them.
    #[test]
    fn an_endpoint_missed_often_enough_is_dropped() {
        let mut list = vec![seeder("corpse", 0.0), seeder("bystander", 0.0)];
        let refusals = [refusal("corpse", true)];
        for _ in 0..super::KNOWN_SEEDER_STRIKE_LIMIT {
            list = super::roster_after_failed_lane(list, &refusals);
        }
        assert_eq!(
            kept(&list),
            vec!["bystander"],
            "a peer nothing has reached in three tries is spent, and only that peer"
        );
    }

    /// Strikes have to be *consecutive*, or a peer that is merely flaky
    /// accumulates its way to deletion over a long session. An answer is
    /// proof of life and puts the count back to zero.
    #[test]
    fn an_answer_wipes_out_earlier_misses() {
        let missed = [refusal("flaky", true)];
        let answered = [refusal("flaky", false)];
        let mut list = vec![seeder("flaky", 0.0)];
        for _ in 0..super::KNOWN_SEEDER_STRIKE_LIMIT - 1 {
            list = super::roster_after_failed_lane(list, &missed);
        }
        list = super::roster_after_failed_lane(list, &answered);
        for _ in 0..super::KNOWN_SEEDER_STRIKE_LIMIT - 1 {
            list = super::roster_after_failed_lane(list, &missed);
        }
        assert_eq!(
            kept(&list),
            vec!["flaky"],
            "the count restarts after an answer instead of resuming"
        );
    }

    fn seeder(endpoint: &str, seen_ms: f64) -> super::KnownSeeder {
        super::KnownSeeder {
            endpoint: endpoint.to_owned(),
            tree: "aaaa".to_owned(),
            seen_ms,
            strikes: 0,
        }
    }

    /// The stored roster survives a round trip, drops entries past the TTL,
    /// and reads garbage as empty rather than failing the connect.
    #[test]
    fn known_seeders_decode_prunes_and_tolerates_garbage() {
        let now = super::KNOWN_SEEDER_TTL_MS * 2.0;
        let stored = vec![
            seeder("fresh", now - 1_000.0),
            seeder("stale", now - super::KNOWN_SEEDER_TTL_MS),
        ];
        let raw = serde_json::to_string(&stored).expect("the roster encodes");
        let decoded = super::decode_known_seeders(&raw, now);
        assert_eq!(decoded, vec![seeder("fresh", now - 1_000.0)]);
        assert!(super::decode_known_seeders("not json", now).is_empty());
    }

    /// Newest first, one record per endpoint, never past the cap.
    #[test]
    fn known_seeders_upsert_dedupes_and_caps() {
        let mut list = Vec::new();
        for n in 0..6 {
            list = super::upsert_known_seeder(list, seeder(&format!("peer-{n}"), f64::from(n)));
        }
        assert_eq!(list.len(), super::KNOWN_SEEDERS_CAP);
        assert_eq!(list[0].endpoint, "peer-5");
        // Re-recording an endpoint moves it to the front instead of
        // duplicating it.
        list = super::upsert_known_seeder(list, seeder("peer-3", 100.0));
        assert_eq!(list.len(), super::KNOWN_SEEDERS_CAP);
        assert_eq!(list[0].endpoint, "peer-3");
        assert_eq!(
            list.iter()
                .filter(|entry| entry.endpoint == "peer-3")
                .count(),
            1
        );
    }

    /// A tab's endpoint ids and the cards the mesh has for it.
    fn swarm_view<'a>(
        local: &'a str,
        ours: &'a std::collections::HashSet<String>,
        producer: &'a str,
        direct: &'a std::collections::HashSet<String>,
        present: &'a [PeerCard],
    ) -> SwarmView<'a> {
        SwarmView {
            local,
            ours,
            producer,
            direct,
            present,
            all: present,
            data_path: "webrtc",
        }
    }

    fn id_set(names: &[&str]) -> std::collections::HashSet<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    fn listed_card(endpoint: &str) -> PeerCard {
        PeerCard::new(
            endpoint,
            "9.9.9",
            "browser",
            "webrtc",
            Some("consumer".to_owned()),
        )
    }

    fn row_pairs(view: &SwarmView<'_>) -> Vec<(String, String)> {
        swarm_rows(
            view,
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
        )
        .into_iter()
        .map(|row| {
            let field = |key: &str| row[key].as_str().unwrap_or_default().to_owned();
            (field("id"), field("role"))
        })
        .collect()
    }

    /// The seeder path mints a second key, so the tab's card is published
    /// under its *mesh* id while the row is keyed by its *mount* id. Reading
    /// only the mount id let the tab's own card fall through as a stranger,
    /// which is half of why the Peers list outnumbered the mesh.
    #[test]
    fn a_tab_with_two_identities_lists_itself_once() {
        let ours = id_set(&["mount-id", "mesh-id"]);
        let direct = id_set(&[]);
        let present = [listed_card("mesh-id"), listed_card("peer-b")];
        let listed = row_pairs(&swarm_view(
            "mount-id",
            &ours,
            "producer-id",
            &direct,
            &present,
        ));
        assert_eq!(
            listed,
            vec![
                ("mount-id".to_owned(), "self".to_owned()),
                ("producer-id".to_owned(), "producer".to_owned()),
                ("peer-b".to_owned(), "gossip".to_owned()),
            ]
        );
    }

    /// And the card follows: our second identity published it, so the self
    /// row must find it there rather than reporting us as an unknown client.
    #[test]
    fn the_self_row_takes_the_card_our_other_identity_published() {
        let ours = id_set(&["mount-id", "mesh-id"]);
        let direct = id_set(&[]);
        let present = [listed_card("mesh-id")];
        let listed = swarm_rows(
            &swarm_view("mount-id", &ours, "producer-id", &direct, &present),
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
        );
        assert_eq!(listed[0]["version"].as_str(), Some("9.9.9"));
    }

    /// One row per peer, whichever source claimed it. A peer that is both a
    /// live channel and a mesh member is one peer, not two rows.
    #[test]
    fn every_row_names_a_different_peer() {
        let ours = id_set(&["mount-id"]);
        let direct = id_set(&["peer-b", "producer-id"]);
        let present = [listed_card("peer-b"), listed_card("peer-c")];
        let listed = row_pairs(&swarm_view(
            "mount-id",
            &ours,
            "producer-id",
            &direct,
            &present,
        ));
        let mut seen: Vec<&str> = listed.iter().map(|(id, _)| id.as_str()).collect();
        let count = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(
            seen.len(),
            count,
            "a peer must not be listed twice: {listed:?}"
        );
    }

    /// The mesh half of the list is the mesh's answer, so a peer it has
    /// dropped produces no row — the other half of the count disagreement.
    #[test]
    fn a_peer_the_mesh_has_dropped_gets_no_row() {
        let ours = id_set(&["mount-id"]);
        let direct = id_set(&[]);
        let listed = row_pairs(&swarm_view("mount-id", &ours, "producer-id", &direct, &[]));
        assert_eq!(listed.len(), 2, "self and the producer only: {listed:?}");
    }

    /// But the rows above the mesh are not the mesh's to take away. A silent
    /// roster must never hide the peer we are streaming from.
    #[test]
    fn the_producer_survives_a_mesh_that_knows_nobody() {
        let ours = id_set(&["mount-id"]);
        let direct = id_set(&["producer-id"]);
        let listed = row_pairs(&swarm_view("mount-id", &ours, "producer-id", &direct, &[]));
        assert!(listed.contains(&("producer-id".to_owned(), "producer".to_owned())));
    }
}
