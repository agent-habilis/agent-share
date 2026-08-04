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

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::rc::Rc;

use fofoca_blobs::{BlobStore, FileId, IdbStore, extent_of};
use std::sync::Arc;

use agent_share_proto::framing::{
    self, BENCH_ECHO_INTERVAL_SECS, DEFAULT_BENCH_DURATION_SECS, MAX_BENCH_ECHO_BYTES,
    MAX_BENCH_FILL_BYTES, MAX_MANIFEST_BYTES, MAX_READ_LEN, MOUNT_ALPN, SECRET_LEN,
    WEBRTC_SIGNAL_ALPN,
};
use agent_share_proto::lookup::{LookupOpts, RelayChoice};
use agent_share_proto::manifest::{ManifestDelta, MountManifest};
use agent_share_proto::mesh_key::share_mesh_key;
use agent_share_proto::ticket::{MountTicket, TICKET_KIND_BENCH_RELAY, TICKET_KIND_BENCH_WEBRTC};
use iroh::endpoint::{Connection, presets};
use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey, TransportAddr};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

mod link;
mod live_state;
mod mesh;
mod produce;
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
    secret: [u8; SECRET_LEN],
    /// `"webrtc"` or `"relay"` — the path that actually carries mount bytes.
    data_path: String,
    /// Requested connect mode (`webrtc` / `relay` / `dynamic`).
    mount_mode: String,
    /// Why `dynamic` ended up on the relay, when it did. `None` on a clean
    /// connect. Surfaced on the info pane: a fallback that only warns to the
    /// console is a fallback nobody can diagnose from the UI.
    fallback_reason: Option<String>,
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
    mesh: RefCell<Option<Rc<mesh::MeshPeer>>>,
    /// Bytes this tab holds, and can therefore seed.
    ///
    /// Opened on the first sync rather than at connect: a tab that only browses
    /// should not create a database, and `IndexedDB` can be refused outright in
    /// private mode — which must cost seeding, never the share.
    store: RefCell<Option<Rc<IdbStore>>>,
    /// Manifest indices fully held, so the UI can mark what is seedable and the
    /// card can advertise it.
    ///
    /// Indices rather than paths because that is what a `READ` addresses and
    /// what the availability grid paints. Recomputed from the store rather than
    /// accumulated, so a reload shows what actually survived instead of what
    /// this session happened to fetch.
    held: RefCell<BTreeSet<u32>>,
}

fn new_share_client(
    connection: Connection,
    secret: [u8; SECRET_LEN],
    data_path: String,
    hub: Option<Arc<BrowserHubTransport>>,
    session: Option<BrowserSession>,
    mesh_endpoint: Option<MeshEndpoint>,
    endpoint: Endpoint,
) -> ShareClient {
    ShareClient {
        connection,
        secret,
        data_path,
        mount_mode: "dynamic".to_owned(),
        fallback_reason: None,
        rendezvous_relays: Vec::new(),
        lookups: LookupOpts::public_preset(),
        connected_at_ms: now_ms(),
        ip_cache: Rc::new(RefCell::new(HashMap::new())),
        bytes_cache: Rc::new(RefCell::new(HashMap::new())),
        link_cache: RefCell::new(HashMap::new()),
        _hub: hub,
        _session: session,
        mesh_endpoint,
        _endpoint: endpoint,
        mesh: RefCell::new(None),
        store: RefCell::new(None),
        held: RefCell::new(BTreeSet::new()),
    }
}

#[wasm_bindgen]
impl ShareClient {
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
    pub async fn connect(
        ticket: String,
        transport: Option<String>,
        card: Option<JsValue>,
    ) -> Result<ShareClient, JsValue> {
        console_error_panic_hook::set_once();
        let mode = TransportMode::parse(transport.as_deref())
            .map_err(|message| JsValue::from_str(&message))?;
        let ticket = MountTicket::decode(&ticket).map_err(|error| err("decode ticket", &error))?;
        let secret = ticket.secret;
        // Captured before `ticket` is moved into the connect. The mesh is
        // derived from the share's own reach, so every holder of this ticket —
        // producer included — computes the same mesh id.
        let lookups = ticket.lookups.clone();
        let mut client = match mode {
            TransportMode::Relay => connect_relay(ticket).await,
            TransportMode::WebRtc => connect_webrtc(ticket, /*allow_relay_fallback=*/ false).await,
            TransportMode::Dynamic => {
                connect_webrtc(ticket, /*allow_relay_fallback=*/ true).await
            }
        }?;
        client.mount_mode = mode.as_str().to_owned();
        client.lookups = lookups.clone();
        client.connected_at_ms = now_ms();
        // Join the share's mesh so this tab can see — and hold direct sessions
        // with — the other people viewing the same share. Strictly additive:
        // a mesh that will not start costs the peer counts and nothing else,
        // so it must never turn a working share into a failed connect.
        let shared = client
            .mesh_endpoint
            .take()
            .map(|shared| (shared.endpoint, shared.webrtc));
        let card = match card.as_ref() {
            Some(value) => {
                mesh::parse_card_parts(value, &client.data_path, Some("consumer".to_owned()))?
            }
            None => mesh::default_card_parts(&client.data_path, Some("consumer".to_owned())),
        };
        match mesh::MeshPeer::join_share(&secret, &lookups, shared, card).await {
            Ok(peer) => *client.mesh.borrow_mut() = Some(Rc::new(peer)),
            Err(error) => {
                web_sys::console::warn_1(&JsValue::from_str(&format!(
                    "[share] mesh unavailable; peer counts disabled: {error:?}"
                )));
            }
        }
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
        let mesh_hub = self
            .mesh
            .borrow()
            .as_ref()
            .map(|peer| Arc::clone(peer.hub()));
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
            let meter = previous.meter.sample(sent as f64, received as f64, rtt_ms, now);
            cache.insert(key.to_owned(), link::LaneMeter { meter, selected });
        };

        let (sent, received) = link::read_quic_total(&self.connection);
        fold(TOTAL_LANE, sent, received, None, false);
        for lane in link::read_quic(&self.connection) {
            fold(&lane.label, lane.sent, lane.received, lane.rtt_ms, lane.selected);
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
        self.mesh.borrow().as_ref().map_or(0, |peer| peer.peers_gossip())
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
        self.mesh.borrow().as_ref().map_or(0, |peer| peer.max_direct())
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
        let Some(peer) = self.mesh.borrow_mut().take() else {
            return;
        };
        wasm_bindgen_futures::spawn_local(async move {
            let _ = peer.leave().await;
        });
    }

    /// Which path carries mount data: `"webrtc"` or `"relay"`.
    #[must_use]
    #[wasm_bindgen(getter)]
    pub fn transport(&self) -> String {
        self.data_path.clone()
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
        self.connection.close(0u32.into(), b"dev: closed from the info pane");
    }

    /// The whole tree, in one shot: `{ dirs: [...], files: [...] }`.
    ///
    /// One request by design — the protocol has no per-directory listing op,
    /// so navigation is instant and only *bytes* are lazy.
    ///
    /// # Errors
    /// The producer refuses the request or the manifest does not decode.
    pub async fn manifest(&self) -> Result<JsValue, JsValue> {
        let (_, manifest) = self.fetch_manifest().await?;
        serde_wasm(&manifest)
    }

    /// The manifest, and the exact bytes it was decoded from.
    ///
    /// The bytes matter separately from the struct: the tree fingerprint is
    /// taken over what the producer actually served, so both sides hash the
    /// same thing rather than trusting a re-encode to be canonical.
    async fn fetch_manifest(&self) -> Result<(Vec<u8>, MountManifest), JsValue> {
        let (mut send, mut recv) = self
            .connection
            .open_bi()
            .await
            .map_err(|error| stream_open_failed("could not fetch the listing", &error))?;
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
        // The card could not carry a tree at join — `join_share` runs from the
        // constructor, before this — so publish it now that we know one.
        //
        // Cloned out of the cell, not borrowed across the await below: see the
        // note on the `mesh` field.
        let mesh = self.mesh.borrow().clone();
        if let Some(mesh) = mesh {
            mesh.set_tree(agent_share_proto::manifest::manifest_fingerprint(&bytes))
                .await;
        }
        Ok((bytes, manifest))
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
        let conn = self.connection.clone();
        let secret = self.secret;
        wasm_bindgen_futures::spawn_local(async move {
            loop {
                match follow_watch(&conn, &secret, &on_manifest).await {
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
            &self.secret,
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
        let (bytes, manifest) = self.fetch_manifest().await?;
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
            let file = file_id(entry);
            if is_held(store.as_ref(), &file).await {
                skipped += 1;
                self.held.borrow_mut().insert(index);
                continue;
            }

            let body = self.read_whole(index, entry.size).await?;
            let ours = store
                .insert_complete(&file, &body)
                .await
                .map_err(|error| err("storing a file", &error))?;
            match self.fetch_hash(index).await? {
                Some(theirs) if theirs != ours => {
                    return Err(JsValue::from_str(&format!(
                        "{} does not match the origin: the bytes were altered in transit, \
                         or the origin is serving content it did not hash",
                        entry.rel_path
                    )));
                }
                Some(_) => verified += 1,
                None => unverified += 1,
            }
            self.held.borrow_mut().insert(index);
            files += 1;
            total += body.len() as u64;
        }

        self.publish_serving(&bytes, &manifest).await;

        let out = serde_json::json!({
            "files": files,
            "bytes": total,
            "verified": verified,
            "unverified": unverified,
            "skipped": skipped,
            "held": self.held.borrow().len(),
        });
        js_sys::JSON::parse(&out.to_string()).map_err(|error| JsValue::from(error))
    }

    /// Manifest indices this tab holds in full, and can seed.
    #[must_use]
    #[wasm_bindgen(getter)]
    pub fn held(&self) -> Vec<u32> {
        self.held.borrow().iter().copied().collect()
    }

    /// Recompute what is held from storage, and republish it.
    ///
    /// Called on mount so a reload shows what survived rather than an empty
    /// grid. Never fails: a tab with no store simply holds nothing.
    pub async fn refresh_held(&self) -> Result<(), JsValue> {
        let Ok((bytes, manifest)) = self.fetch_manifest().await else {
            return Ok(());
        };
        // Deliberately does *not* create a database — only adopts one already
        // there. Browsing a share must not leave storage behind.
        let Ok(store) = IdbStore::open(&self.store_name()).await else {
            return Ok(());
        };
        let mut held = BTreeSet::new();
        for (index, entry) in manifest.files.iter().enumerate() {
            if entry.is_tombstone() {
                continue;
            }
            if is_held(&store, &file_id(entry)).await
                && let Ok(index) = u32::try_from(index)
            {
                held.insert(index);
            }
        }
        *self.held.borrow_mut() = held;
        *self.store.borrow_mut() = Some(Rc::new(store));
        self.publish_serving(&bytes, &manifest).await;
        Ok(())
    }

    /// Where this share's blocks live.
    ///
    /// Keyed by the mesh id, which is a one-way hash of the secret — so two
    /// shares never share a database, and the secret itself never reaches a
    /// name that storage inspectors or `about:` pages would display.
    fn store_name(&self) -> String {
        format!(
            "agent-share/{}",
            &agent_share_proto::mesh_key::share_mesh_key(&self.secret)[..16]
        )
    }

    async fn open_store(&self) -> Result<Rc<IdbStore>, JsValue> {
        if let Some(store) = self.store.borrow().as_ref() {
            return Ok(Rc::clone(store));
        }
        let store = Rc::new(
            IdbStore::open(&self.store_name())
                .await
                .map_err(|error| err("opening local storage", &error))?,
        );
        *self.store.borrow_mut() = Some(Rc::clone(&store));
        Ok(store)
    }

    /// Tell the mesh which slots this tab can serve.
    ///
    /// Both fields together: an index means nothing without agreeing which
    /// manifest it indexes into, so a `serving` set published against the wrong
    /// tree would send readers to the wrong files.
    async fn publish_serving(&self, manifest_bytes: &[u8], manifest: &MountManifest) {
        // Cloned out of the cell, not borrowed across the two awaits below:
        // see the note on the `mesh` field.
        let mesh = self.mesh.borrow().clone();
        let Some(mesh) = mesh else {
            return;
        };
        mesh.set_tree(agent_share_proto::manifest::manifest_fingerprint(
            manifest_bytes,
        ))
        .await;
        let held: Vec<u32> = self.held.borrow().iter().copied().collect();
        mesh.set_serving(agent_share_proto::serving::encode_serving(
            &held,
            manifest.files.len(),
        ))
        .await;
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

    /// The root the origin published for `index`, if it can vouch for one.
    async fn fetch_hash(&self, index: u32) -> Result<Option<[u8; 32]>, JsValue> {
        let (mut send, mut recv) = self
            .connection
            .open_bi()
            .await
            .map_err(|error| err("open hash stream", &error))?;
        send.write_all(&framing::encode_hash_request(&self.secret, index))
            .await
            .map_err(|error| err("send hash request", &error))?;
        send.finish().map_err(|error| err("finish", &error))?;

        let mut status = [0u8; 1];
        recv.read_exact(&mut status)
            .await
            .map_err(|error| err("read hash status", &error))?;
        // Anything but Ok means "cannot vouch", which is ordinary — the origin
        // hashes lazily. Only a protocol failure is an error.
        if status[0] != agent_share_proto::manifest::ReadStatus::Ok.to_byte() {
            return Ok(None);
        }
        let mut root = [0u8; 32];
        recv.read_exact(&mut root)
            .await
            .map_err(|error| err("read root", &error))?;
        Ok(Some(root))
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
            TICKET_KIND_BENCH_RELAY => connect_relay_only(ticket).await?,
            TICKET_KIND_BENCH_WEBRTC => {
                connect_webrtc(ticket, /*allow_relay_fallback=*/ false).await?
            }
            _ => unreachable!("validated above"),
        };
        let connect_ms = now_ms() - connect_start;
        emit_status(
            on_status.as_ref(),
            &serde_json::json!({
                "stage": "connected",
                "transport": client.data_path,
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
            transport: client.data_path.clone(),
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
    fn info_json(&self) -> serde_json::Value {
        let producer = self.connection.remote_id().to_string();
        let local = self._endpoint.id().to_string();
        let mesh_up = self.mesh.borrow().is_some();
        let nickname = self.mesh.borrow().as_ref().map(|m| m.nickname());
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
        let live_path =
            selected_path_label(&self.connection).unwrap_or_else(|| self.data_path.clone());
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
                "identity_fingerprint": identity_fingerprint(&self.secret),
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
                "mount_fallback_reason": self.fallback_reason,
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
            .mesh
            .borrow()
            .as_ref()
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

    fn swarm_peers_json(&self, local: &str, producer: &str) -> Vec<serde_json::Value> {
        let cache = self.ip_cache.borrow();
        let bytes = self.bytes_cache.borrow();
        // Self label comes from the meta card the TS consumer published; this
        // is only a last-resort row if the book has not been seeded yet.
        let self_card = agent_share_proto::PeerCard::new(
            local,
            env!("CARGO_PKG_VERSION"),
            "browser",
            &self.data_path,
            Some("consumer".to_owned()),
        );
        let card_for = |id: &str| self.mesh.borrow().as_ref().and_then(|m| m.card_for(id));
        let peer_row = |id: &str,
                        role: &str,
                        flags: String,
                        fallback_proto: &str,
                        ip: Option<String>,
                        ip_kind: Option<String>| {
            let card = card_for(id);
            let client = card.as_ref().map(|c| c.client.clone()).unwrap_or_else(|| {
                if id == local {
                    self_card.client.clone()
                } else {
                    "unknown".to_owned()
                }
            });
            let proto = card
                .as_ref()
                .map(|c| c.transport.clone())
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| fallback_proto.to_owned());
            let version = card.as_ref().map(|c| c.version.clone());
            let runtime = card.as_ref().map(|c| c.runtime.clone());
            let app_role = card.as_ref().and_then(|c| c.role.clone());
            // Availability, for the grid. `tree` rides along because a slot
            // index means nothing without agreeing which manifest it indexes
            // into — two peers on different trees must not be drawn as though
            // their squares line up.
            let serving = card.as_ref().and_then(|c| c.serving.clone());
            let tree = card.as_ref().and_then(|c| c.tree.clone());
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
        let (local_ip, local_ip_kind) = cache.get(local).cloned().unwrap_or((None, None));
        rows.push(peer_row(
            local,
            "self",
            "*".to_owned(),
            &self.data_path,
            local_ip,
            local_ip_kind,
        ));

        // Both hubs: the producer's session lives in the mount's, every other
        // direct peer in the mesh's. Reading one would drop the `D` flag off
        // whichever half it missed.
        let live: Vec<String> = self.direct_peer_ids().into_iter().collect();
        let producer_direct = live.iter().any(|id| id == producer);
        let (ip, ip_kind) = cache.get(producer).cloned().unwrap_or((None, None));
        let mut flags = String::from("S");
        if producer_direct {
            flags.push('D');
        }
        rows.push(peer_row(
            producer,
            "producer",
            flags,
            &self.data_path,
            ip,
            ip_kind,
        ));

        let mut seen: HashSet<String> = HashSet::new();
        seen.insert(local.to_owned());
        seen.insert(producer.to_owned());

        for id in live {
            if !seen.insert(id.clone()) {
                continue;
            }
            let (ip, ip_kind) = cache.get(&id).cloned().unwrap_or((None, None));
            rows.push(peer_row(
                &id,
                "direct",
                "D".to_owned(),
                "webrtc",
                ip,
                ip_kind,
            ));
        }

        // Gossip-only members publish meta cards but may never open a direct
        // hub session — still show them so the Peers list matches the roster.
        let mesh_ref = self.mesh.borrow();
        if let Some(mesh) = mesh_ref.as_ref() {
            for card in mesh.known_cards() {
                if !seen.insert(card.endpoint.clone()) {
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
        }

        rows
    }
}

fn identity_fingerprint(secret: &[u8; SECRET_LEN]) -> String {
    let key = share_mesh_key(secret);
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
    let request = framing::encode_bench_echo_request(&client.secret, &payload)
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
    let request = framing::encode_bench_fill_request(&client.secret, want)
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

/// Dial mount over the ticket address (IP and/or iroh relay). No WebRTC.
async fn connect_relay(ticket: MountTicket) -> Result<ShareClient, JsValue> {
    ensure_reachable_addr(&ticket.addr)?;
    let key = SecretKey::generate();
    let endpoint = Endpoint::builder(presets::Minimal)
        .secret_key(key)
        .relay_mode(relay_mode(&ticket))
        .bind()
        .await
        .map_err(|error| err("bind relay endpoint", &error))?;

    let connection = endpoint
        .connect(ticket.addr.clone(), MOUNT_ALPN)
        .await
        .map_err(|error| err("dial the mount ALPN over iroh relay/IP", &error))?;

    Ok(new_share_client(
        connection,
        ticket.secret,
        "relay".to_owned(),
        None,
        None,
        None,
        endpoint,
    ))
}

/// Dial mount using **only** the ticket's relay URL(s) — no direct IP.
async fn connect_relay_only(ticket: MountTicket) -> Result<ShareClient, JsValue> {
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
        .relay_mode(relay_mode(&ticket))
        .bind()
        .await
        .map_err(|error| err("bind relay-only endpoint", &error))?;

    let connection = dial_with_retry(&endpoint, relay_only).await?;
    ensure_relay_selected(&connection).await?;

    Ok(new_share_client(
        connection,
        ticket.secret,
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
    secret: &[u8; SECRET_LEN],
    on_manifest: &js_sys::Function,
) -> WatchEnd {
    let Ok((mut send, mut recv)) = conn.open_bi().await else {
        return WatchEnd::Retryable;
    };
    if send
        .write_all(&framing::encode_watch_request(secret))
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
fn file_id(entry: &agent_share_proto::manifest::FileEntry) -> FileId {
    FileId {
        key: entry.rel_path.clone(),
        size: entry.size,
        mtime: entry.mtime,
    }
}

/// Whether the store holds every byte of this file version.
///
/// Anything short of complete reads as not held: a partially fetched file
/// cannot be handed to a reader as a file, and advertising it whole would send
/// them somewhere that cannot answer.
async fn is_held(store: &IdbStore, file: &FileId) -> bool {
    let Ok(Some(root)) = store.bind(file).await else {
        return false;
    };
    let Ok(present) = store.present(root).await else {
        return false;
    };
    extent_of(file.size).is_subset(&present)
}

async fn wait_ms(millis: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        if let Some(window) = web_sys::window() {
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, millis);
        }
    });
    let _ = JsFuture::from(promise).await;
}

/// Signal + WebRTC mount dial; optionally fall back to iroh relay/IP.
async fn connect_webrtc(
    ticket: MountTicket,
    allow_relay_fallback: bool,
) -> Result<ShareClient, JsValue> {
    let producer = ticket.addr.id;
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
    // Both endpoints bind the *same* secret key, which is what makes this
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
    let signal_endpoint = Endpoint::builder(presets::Minimal)
        .secret_key(key.clone())
        .relay_mode(relay_mode(&ticket))
        .add_custom_transport(mesh_handle.transport())
        .path_selector(mesh_handle.path_selector())
        .bind()
        .await
        .map_err(|error| err("bind signal endpoint", &error))?;

    let hub = BrowserHubTransport::new(local);
    let handle = WebRtcHandle::new(Arc::clone(&hub));
    let endpoint = Endpoint::builder(presets::Minimal)
        .secret_key(key)
        // No relay, deliberately: this endpoint's whole purpose is to have no
        // path to lose the mount dial to.
        .relay_mode(RelayMode::Disabled)
        .add_custom_transport(handle.transport())
        .path_selector(handle.path_selector())
        .bind()
        .await
        .map_err(|error| err("bind mount endpoint", &error))?;

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
            return finish_relay_fallback(signal_endpoint, ticket, reason).await;
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
            // The *selected* path, not "is a WebRTC path present". Scanning
            // every path with `any()` answered a different question than the
            // one that matters — a connection can hold a WebRTC path it does
            // not send on — so it could report `webrtc` while the relay
            // carried the bytes, and the reverse.
            let selected = settled_path_label(&connection).await;
            let on_webrtc = selected.as_deref() == Some("webrtc");
            if !on_webrtc && !allow_relay_fallback {
                let observed = path_labels(&connection);
                endpoint.close().await;
                signal_endpoint.close().await;
                return Err(JsValue::from_str(&format!(
                    "mount connected but selected {} rather than WebRTC (paths={observed:?}), \
                     and webrtc mode forbids a fallback",
                    selected.as_deref().unwrap_or("no path"),
                )));
            }
            // Captured before the endpoint moves into the mesh below.
            let rendezvous_relays: Vec<String> = signal_endpoint
                .addr()
                .relay_urls()
                .map(|url| url.to_string())
                .collect();
            let mut client = new_share_client(
                connection,
                ticket.secret,
                // Report what was actually selected. Previously this said
                // "webrtc" unconditionally on this path, which was a guess.
                selected.clone().unwrap_or_else(|| "relay".to_owned()),
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
            if !on_webrtc {
                // With no relay and no IP on this endpoint there is nothing for
                // the mount to settle on *but* WebRTC, so this is now a
                // "selection never settled" report rather than a lost race.
                client.fallback_reason = Some(format!(
                    "the mount reported {} rather than WebRTC on a relay-free endpoint",
                    selected
                        .as_deref()
                        .unwrap_or("no path before the settle deadline"),
                ));
            }
            Ok(client)
        }
        Err(error) if allow_relay_fallback => {
            let reason = format!("WebRTC mount dial failed: {error}");
            web_sys::console::warn_1(&JsValue::from_str(&format!(
                "[agent-share] {reason}; falling back to iroh relay/IP"
            )));
            endpoint.close().await;
            finish_relay_fallback(signal_endpoint, ticket, reason).await
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
    reason: String,
) -> Result<ShareClient, JsValue> {
    let connection = endpoint
        .connect(ticket.addr.clone(), MOUNT_ALPN)
        .await
        .map_err(|error| err("dial the mount ALPN over iroh relay/IP (fallback)", &error))?;
    let mut client = new_share_client(
        connection,
        ticket.secret,
        "relay".to_owned(),
        None,
        None,
        None,
        endpoint,
    );
    client.fallback_reason = Some(reason);
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
    local: iroh_base::EndpointId,
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
    let conn = endpoint
        .connect(producer, WEBRTC_SIGNAL_ALPN)
        .await
        .map_err(|error| err("dial the signal ALPN", &error))?;
    let (mut send, mut recv) = conn
        .open_bi()
        .await
        .map_err(|error| err("open signal stream", &error))?;

    // STUN only — TURN is refused by policy; the iroh relay is this
    // project's relay, and running a second one at the ICE layer would mean
    // operating two systems for one job.
    let ice = IceServers::default();
    let (pending, offer) = browser_offer(local, &ice)
        .await
        .map_err(|error| js_stage("build offer", error))?;
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
async fn read_header(recv: &mut iroh::endpoint::RecvStream, cap: u32) -> Result<u32, JsValue> {
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
fn stream_open_failed(what: &str, error: &iroh::endpoint::ConnectionError) -> JsValue {
    if matches!(error, iroh::endpoint::ConnectionError::TimedOut) {
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
fn pinned_ladder() -> Vec<iroh::RelayUrl> {
    agent_habilis_mesh::RENDEZVOUS_RELAY_LADDER
        .iter()
        .map(|raw| {
            raw.parse()
                .expect("RENDEZVOUS_RELAY_LADDER entries are valid relay URLs")
        })
        .collect()
}

