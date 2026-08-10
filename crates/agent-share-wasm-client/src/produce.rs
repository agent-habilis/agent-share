//! In-browser share producer: serve a File System Access tree over WebRTC.
//!
//! Live: JS rescans and calls [`ShareProducer::update`]; `OP_WATCH` pushes
//! full-manifest frames. The accept loop answers the signal ALPN (JSEP
//! answerer) and the mount ALPN (manifest / read / watch).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use agent_share_proto::auth::ShareAuth;
use agent_share_proto::framing::{
    BENCH_KIND_ECHO, BENCH_KIND_FILL, MAX_BENCH_ECHO_BYTES, MAX_BENCH_FILL_BYTES,
    MAX_MANIFEST_BYTES, MAX_READ_LEN, MOUNT_ALPN, OP_BENCH, OP_CHUNK, OP_CHUNK_MAP, OP_HAVE,
    REQUEST_HEADER_LEN, SECRET_LEN, WATCH_FRAME_MANIFEST, WEBRTC_SIGNAL_ALPN,
    decode_bench_request_prefix,
};
use agent_share_mount::{ServeSource, Watcher, serve_stream};
use agent_share_proto::authorship::{SIGNATURE_LEN, SignedManifest};
use agent_share_proto::lookup::LookupOpts;
use agent_share_proto::manifest::{DirEntry, FileEntry, ReadStatus};
use agent_share_proto::ticket::{MountTicket, TICKET_KIND_BENCH_RELAY, TICKET_KIND_BENCH_WEBRTC};
use fofoca::iroh::endpoint::{Connection, presets};
use fofoca::iroh::{Endpoint, SecretKey};
use fofoca_iroh_webrtc_transport::{
    BrowserHubTransport, IceServers, MAX_ENVELOPE_BYTES, SignalEnvelope, WebRtcHandle,
    browser_answer, log_signal_sdps,
};
use futures::StreamExt as _;
use futures::channel::mpsc;
use futures::channel::oneshot;
use js_sys::{Array, Reflect, Uint8Array};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::FileSystemFileHandle;

use fofoca_chunks::{
    CHUNK_BYTES, ChunkHash, ChunkMap, ChunkMapBuilder, Coverage, Root, chunk_hash,
};
use std::collections::HashMap;

use crate::live_state::LiveState;

/// Chunk rows for files somebody has asked about, plus the reverse index that
/// lets a bare address find its way back to a slot.
///
/// Built lazily, exactly like the native origin's: a tab sharing a folder does
/// not read a byte until a peer asks about a specific file, so publishing a
/// large directory stays instant.
#[derive(Default)]
struct ChunkTable {
    rows: HashMap<u32, ChunkMap>,
    slot_of_root: HashMap<Root, u32>,
    /// Address → the slot and position it was computed from. This is what makes
    /// answering a bare `OP_CHUNK` possible for a source that owns no bytes.
    slot_of_address: HashMap<ChunkHash, (u32, usize)>,
}

impl ChunkTable {
    /// Forget everything derived from `index`, because the file behind it moved.
    fn forget(&mut self, index: u32) {
        if let Some(row) = self.rows.remove(&index) {
            self.slot_of_root.remove(&row.root());
            for leaf in row.leaves() {
                // Only drop an address that still points at this slot: two
                // files sharing a chunk both registered it, and whichever
                // claimed it first is still able to answer.
                if self.slot_of_address.get(leaf).is_some_and(|(slot, _)| *slot == index) {
                    self.slot_of_address.remove(leaf);
                }
            }
        }
    }

    fn insert(&mut self, index: u32, row: ChunkMap) {
        self.forget(index);
        self.slot_of_root.insert(row.root(), index);
        for (position, leaf) in row.leaves().iter().enumerate() {
            self.slot_of_address.entry(*leaf).or_insert((index, position));
        }
        self.rows.insert(index, row);
    }
}

pub(crate) struct ProducerShared {
    state: LiveState<FileSystemFileHandle>,
    watchers: Vec<mpsc::UnboundedSender<Rc<Vec<u8>>>>,
    chunks: ChunkTable,
}

type Shared = Rc<RefCell<ProducerShared>>;

/// The browser's end of a watch feed.
///
/// The trait it satisfies lives in [`agent_share_mount`], shared with the CLI,
/// which broadcasts `Arc<Vec<u8>>` over tokio instead. The frame type is the
/// one thing that could not be shared, so it is the one thing named here.
pub(crate) struct FrameFeed(pub(crate) mpsc::UnboundedReceiver<Rc<Vec<u8>>>);

impl Watcher for FrameFeed {
    /// The sender's own `Rc`, so a frame is never copied to cross the seam.
    type Frame = Rc<Vec<u8>>;

    async fn recv(&mut self) -> Option<Self::Frame> {
        self.0.next().await
    }
}

/// A watch registration: the opening frame, then the update stream.
pub(crate) type WatchFeed = (Vec<u8>, FrameFeed);

/// A share served from a File System Access tree.
///
/// A newtype over [`Shared`] rather than an impl on it: [`ServeSource`] is
/// `agent_share_mount`'s now, and a foreign trait cannot be implemented for
/// `Rc<RefCell<…>>`. It also puts the browser and the CLI on the same shape —
/// each has a `ProducerSource` wrapping whatever it reads through to.
#[derive(Clone)]
pub(crate) struct ProducerSource(Shared);

impl ProducerSource {
    pub(crate) fn new(shared: Shared) -> Self {
        Self(shared)
    }
}

impl ServeSource for ProducerSource {
    type Watcher = FrameFeed;

    /// A browser-produced share is **unsigned**: there is no key here to sign
    /// with and nowhere durable to keep one, so the envelope carries a zero
    /// signature and the ticket this producer hands out names no author. A
    /// reader therefore never checks, which is the honest outcome — the
    /// alternative would be a signature nobody could attribute to anyone.
    fn manifest_envelope(&self) -> Option<Vec<u8>> {
        Some(
            SignedManifest {
                version: 0,
                signature: [0u8; SIGNATURE_LEN],
                manifest: self.0.borrow().state.encoded().to_vec(),
            }
            .encode(),
        )
    }

    fn subscribe(&self) -> Option<WatchFeed> {
        let (tx, rx) = mpsc::unbounded::<Rc<Vec<u8>>>();
        let mut borrowed = self.0.borrow_mut();
        borrowed.watchers.push(tx);
        let mut frame = Vec::with_capacity(1 + borrowed.state.encoded().len());
        frame.push(WATCH_FRAME_MANIFEST);
        frame.extend_from_slice(borrowed.state.encoded());
        Some((frame, FrameFeed(rx)))
    }

    async fn answer_read(&self, index: u32, offset: u64, len: u32) -> (ReadStatus, Vec<u8>) {
        answer_read(&self.0, index, offset, len).await
    }

    async fn answer_chunk_map(&self, index: u32) -> Option<ChunkMap> {
        producer_chunk_map(&self.0, index).await
    }

    async fn answer_chunk(&self, address: ChunkHash) -> Option<Vec<u8>> {
        producer_chunk(&self.0, address).await
    }

    async fn answer_have(&self, root: Root) -> Option<Coverage> {
        let index = { self.0.borrow().chunks.slot_of_root.get(&root).copied()? };
        // Recomputing rather than trusting the cached row is what notices a
        // file that moved: if it did, the row is rebuilt under a new root and
        // this one stops being served at all.
        let row = producer_chunk_map(&self.0, index).await?;
        if row.root() != root {
            return None;
        }
        Some(Coverage::complete(row.len()))
    }
}

/// The leaf row for `index`, reading the file once if this is the first ask.
///
/// Streamed in chunk-sized pieces rather than slurped: a tab sharing a 4 GB
/// video must not be asked to hold it in memory to describe it.
async fn producer_chunk_map(shared: &Shared, index: u32) -> Option<ChunkMap> {
    let handle = {
        let borrowed = shared.borrow();
        borrowed.state.slot(index)?.clone()
    };
    // The live size is the version gate. A cached row describing a different
    // length is a file that moved, and answering from it would hand out
    // addresses whose bytes are gone.
    let live = live_size(&handle).await?;
    if let Some(row) = shared.borrow().chunks.rows.get(&index)
        && row.size() == live
    {
        return Some(row.clone());
    }

    let mut builder = ChunkMapBuilder::new();
    let mut offset = 0u64;
    while offset < live {
        let want = u32::try_from((live - offset).min(CHUNK_BYTES)).ok()?;
        let piece = read_from_handle(&handle, offset, want).await.ok()?;
        if piece.is_empty() {
            // The file shrank while being read; the row would describe neither
            // version, so there is nothing honest to answer with.
            return None;
        }
        offset += piece.len() as u64;
        builder.push(&piece);
    }
    let row = builder.finish();
    if row.size() != live {
        return None;
    }
    shared.borrow_mut().chunks.insert(index, row.clone());
    Some(row)
}

/// The bytes at one address, read back out of the file they came from.
async fn producer_chunk(shared: &Shared, address: ChunkHash) -> Option<Vec<u8>> {
    let (index, position) = { shared.borrow().chunks.slot_of_address.get(&address).copied()? };
    let handle = {
        let borrowed = shared.borrow();
        borrowed.state.slot(index)?.clone()
    };
    let range = {
        let borrowed = shared.borrow();
        borrowed.chunks.rows.get(&index)?.range_of(position)
    };
    let want = u32::try_from(range.end - range.start).ok()?;
    let bytes = read_from_handle(&handle, range.start, want).await.ok()?;
    // Re-verify before answering. The file is the user's and can change between
    // the row being built and this read; serving unverified bytes is how a tab
    // becomes the peer everyone else has to defend against.
    if chunk_hash(&bytes) != address {
        shared.borrow_mut().chunks.forget(index);
        return None;
    }
    Some(bytes)
}

/// The file's size right now, straight from the handle.
async fn live_size(handle: &FileSystemFileHandle) -> Option<u64> {
    let file = JsFuture::from(handle.get_file()).await.ok()?;
    let file: web_sys::File = file.dyn_into().ok()?;
    Some(file.size() as u64)
}

/// An in-browser share, serving until [`ShareProducer::stop`].
#[wasm_bindgen]
pub struct ShareProducer {
    ticket: String,
    shared: Shared,
    /// This tab's mesh membership. It also owns the Router serving the share's
    /// own ALPNs, so dropping it stops the share serving.
    ///
    /// Behind a `RefCell` so [`ShareProducer::stop`] can take `&self`. See the
    /// note there: a `self`-by-value method is a trap through wasm-bindgen.
    mesh: RefCell<Option<crate::mesh::MeshPeer>>,
    /// Latched by the first `stop`, so a second one is a no-op rather than a
    /// concurrent teardown racing the first.
    stopped: Cell<bool>,
    endpoint: Endpoint,
    hub: Arc<BrowserHubTransport>,
}

#[wasm_bindgen]
impl ShareProducer {
    /// Start serving a pre-scanned listing from JS:
    /// `{ dirs: string[], files: { rel_path, size, mtime, handle }[] }`.
    ///
    /// Pass a `password` to protect the share: the ticket then addresses it
    /// without opening it, so the link is safe to post somewhere the password
    /// is not. Costs ~100 ms of Argon2id on the main thread, once, here.
    ///
    /// # Errors
    /// Bad listing shape, bind failure, or empty tree.
    pub async fn start(
        listing: JsValue,
        card: Option<JsValue>,
        password: Option<String>,
    ) -> Result<ShareProducer, JsValue> {
        console_error_panic_hook::set_once();
        let scanned = parse_listing(&listing)?;
        if scanned.files.is_empty() && scanned.dirs.is_empty() {
            return Err(JsValue::from_str("share is empty"));
        }

        let state = LiveState::new(scanned.dirs, scanned.files);
        if state.encoded().len() > MAX_MANIFEST_BYTES as usize {
            return Err(JsValue::from_str("tree too large to serve"));
        }

        let shared: Shared = Rc::new(RefCell::new(ProducerShared {
            state,
            watchers: Vec::new(),
            chunks: ChunkTable::default(),
        }));

        let key = SecretKey::generate();
        let local = key.public();
        let hub = BrowserHubTransport::new(local);
        let handle = WebRtcHandle::new(Arc::clone(&hub));

        // A tab is always publicly reachable or not reachable at all — it has no
        // mDNS, no DHT, and no loopback peers. Hoisted above the bind so the
        // endpoint, the mesh derivation, and the ticket cannot state different
        // reaches: they must agree, or a viewer derives a mesh the producer is
        // not on, or dials a relay ladder this tab never homed on.
        let lookups = LookupOpts::public_preset();

        let endpoint = Endpoint::builder(presets::Minimal)
            .secret_key(key)
            .relay_mode(crate::relay_mode(&lookups.relay))
            .alpns(vec![MOUNT_ALPN.to_vec(), WEBRTC_SIGNAL_ALPN.to_vec()])
            .add_custom_transport(handle.transport())
            .path_selector(handle.path_selector())
            .bind()
            .await
            .map_err(|error| err("bind producer endpoint", &error))?;

        let mut secret = [0u8; SECRET_LEN];
        getrandom::fill(&mut secret).map_err(|error| err("mint secret", &error))?;
        // The share's real credential. Without a password it *is* the secret,
        // so an ordinary browser share is byte-for-byte what it was.
        let auth = ShareAuth::new(&secret, password.as_deref());

        // The mesh's Router owns `accept()` now, and it is up before the
        // ticket exists — so a fast joiner still is not raced. Injecting the
        // endpoint keeps `setup_mesh` cheap: no key to mint, no second bind,
        // and no second relay registration.
        let protocols: Vec<(Vec<u8>, Box<dyn fofoca::iroh::protocol::DynProtocolHandler>)> = vec![
            (
                MOUNT_ALPN.to_vec(),
                Box::new(MountHandler::new(ProducerSource::new(Rc::clone(&shared)), auth)),
            ),
            (
                WEBRTC_SIGNAL_ALPN.to_vec(),
                Box::new(SignalHandler::new(local, Arc::clone(&hub))),
            ),
        ];
        // One identity for this tab: the mount peer and the mesh peer are the
        // same node, so a viewer counts this producer once rather than twice.
        let card = match card.as_ref() {
            Some(value) => {
                crate::mesh::parse_card_parts(value, "webrtc", Some("producer".to_owned()))?
            }
            None => crate::mesh::default_card_parts("webrtc", Some("producer".to_owned())),
        };
        // Minted here so the id can go in the ticket. On a protected share it
        // carries the verifier fofoca baked in, which is what lets a viewer rule
        // on its password locally instead of asking this producer — which may
        // not be running when they try.
        let mesh_id = crate::mesh::mint_share_mesh_id(&secret, &lookups, password.as_deref())?;
        let resolved = crate::mesh::resolve_share(crate::mesh::ShareMeshRef {
            mesh_id: Some(&mesh_id),
            secret: &secret,
            lookups: &lookups,
            password: password.as_deref(),
        })?;
        let mesh = crate::mesh::MeshPeer::join_share_with(
            resolved,
            endpoint.clone(),
            handle.clone(),
            protocols,
            card,
        )
        .await?;

        // Ticket must carry a relay URL — the browser consumer has no mdns/dht
        // and dials the signal ALPN from `ticket.addr` alone.
        wait_until_dialable(&endpoint).await?;
        let ticket = MountTicket {
            addr: endpoint.addr(),
            secret,
            lookups,
            kind: agent_share_proto::ticket::TICKET_KIND_SHARE,
            flags: if auth.password_protected() {
                agent_share_proto::ticket::TICKET_FLAG_PASSWORD
            } else {
                0
            },
            // Only on a protected share: an ordinary one needs no id in its
            // ticket, because every peer derives the same mesh from the secret.
            mesh_id: auth.password_protected().then(|| mesh_id.clone()),
            author: None,
        };
        let ticket_str = ticket.encode();

        Ok(ShareProducer {
            ticket: ticket_str,
            shared,
            mesh: RefCell::new(Some(mesh)),
            stopped: Cell::new(false),
            endpoint,
            hub,
        })
    }

    /// Fold a fresh directory listing into the live tree and notify watchers.
    ///
    /// Sync on purpose: no awaits ⇒ atomic with respect to reads and watch
    /// registration. Oversized encodings are refused; the previous tree stays.
    ///
    /// # Errors
    /// Bad listing shape.
    pub fn update(&self, listing: JsValue) -> Result<(), JsValue> {
        let scanned = parse_listing(&listing)?;
        let mut shared = self.shared.borrow_mut();
        let previous = shared.state.snapshot();
        if !shared.state.apply(scanned.dirs, scanned.files) {
            return Ok(());
        }
        if shared.state.encoded().len() > MAX_MANIFEST_BYTES as usize {
            web_sys::console::warn_1(&JsValue::from_str(
                "[share] rescan exceeded MAX_MANIFEST_BYTES; keeping previous tree",
            ));
            shared.state.restore(previous);
            return Ok(());
        }
        let mut frame = Vec::with_capacity(1 + shared.state.encoded().len());
        frame.push(WATCH_FRAME_MANIFEST);
        frame.extend_from_slice(shared.state.encoded());
        let frame = Rc::new(frame);
        shared
            .watchers
            .retain(|tx| tx.unbounded_send(Rc::clone(&frame)).is_ok());
        Ok(())
    }

    #[wasm_bindgen(getter)]
    pub fn ticket(&self) -> String {
        self.ticket.clone()
    }

    #[wasm_bindgen(getter)]
    pub fn transport(&self) -> String {
        "webrtc".to_owned()
    }

    #[wasm_bindgen(getter)]
    pub fn files(&self) -> u32 {
        self.shared.borrow().state.live_counts().0
    }

    #[wasm_bindgen(getter)]
    pub fn bytes(&self) -> u64 {
        self.shared.borrow().state.live_counts().1
    }

    /// Members on this share's mesh, including us.
    #[wasm_bindgen(getter)]
    pub fn peers_gossip(&self) -> u32 {
        self.mesh
            .borrow()
            .as_ref()
            .map_or(0, crate::mesh::MeshPeer::peers_gossip)
    }

    /// Peers we hold a direct `WebRTC` data channel with — viewers of this
    /// share included, since the mount sessions now live in the same hub.
    #[wasm_bindgen(getter)]
    pub fn peers_direct(&self) -> u32 {
        self.mesh
            .borrow()
            .as_ref()
            .map_or(0, crate::mesh::MeshPeer::peers_direct)
    }

    /// The direct-session ceiling this tab negotiates up to.
    #[wasm_bindgen(getter)]
    pub fn max_direct(&self) -> u32 {
        self.mesh
            .borrow()
            .as_ref()
            .map_or(0, crate::mesh::MeshPeer::max_direct)
    }

    /// Stop accepting peers. Idempotent, and that is load-bearing.
    ///
    /// Leaving the mesh is what stops the share serving now: the mesh's Router
    /// owns the accept loop for this tab's ALPNs, so dropping it is the
    /// shutdown. It also broadcasts `Left`, which the old stop channel never
    /// did — peers used to wait out a silence timeout.
    ///
    /// `&self`, not `self`. A `self`-by-value method compiles to a
    /// `__destroy_into_raw()` in the wasm-bindgen glue, which nulls the JS
    /// object's pointer — so a second call passes `0` to Rust, panics with
    /// "null pointer passed to rust", and, because this crate builds with
    /// `panic = "abort"`, traps the whole wasm instance. Every later call into
    /// the module then throws "unreachable executed". Two ordinary UI paths
    /// reach a second call: double-clicking Stop, and clicking Stop then
    /// changing the hash before the await resolves.
    ///
    /// The latch is taken synchronously, before the first await, so a second
    /// call cannot tear down concurrently with the first either.
    pub async fn stop(&self) -> Result<(), JsValue> {
        if self.stopped.replace(true) {
            return Ok(());
        }
        // Scoped so the `RefMut` is dropped before the await — a borrow held
        // across a suspension point is how single-threaded code still manages
        // to hit `already borrowed`.
        let mesh = self.mesh.borrow_mut().take();
        if let Some(mesh) = mesh {
            mesh.leave().await?;
        }
        // Close live WebRTC sessions before the endpoint: dropping them clears
        // their browser handlers and closes the peer connections.
        self.hub.detach_all();
        self.endpoint.close().await;
        Ok(())
    }
}

/// The producer's two protocols, as `ProtocolHandler`s on the mesh's Router.
///
/// The producer tab used to own `endpoint.accept()`. It cannot any more: the
/// share and the mesh share one endpoint, and iroh allows exactly one accept
/// loop per endpoint — `Router::spawn` overrides the ALPN list, and two loops
/// race for one queue.
///
/// Both handlers must be `Send + Sync + 'static` to live in the Router, while
/// the producer's state holds `FileSystemFileHandle`s and the JSEP path holds
/// web-sys closures — all `!Send`. `SendWrapper` bridges that: it is sound
/// because wasm is single-threaded, and it panics loudly rather than silently
/// if that ever stops being true. The actual work is then spawned with
/// `n0_future::task::spawn`, which is `spawn_local` here, so the `!Send` future
/// never has to satisfy the Router's `Send` accept signature.
#[derive(Clone)]
pub(crate) struct MountHandler<S> {
    source: send_wrapper::SendWrapper<S>,
    /// What an inbound request must present, and how to refuse one that does
    /// not. Identical to the native producer's, from the same crate — a browser
    /// tab seeding a share is a producer, not a special case.
    auth: ShareAuth,
}

impl<S> MountHandler<S> {
    pub(crate) fn new(source: S, auth: ShareAuth) -> Self {
        Self {
            source: send_wrapper::SendWrapper::new(source),
            auth,
        }
    }
}

impl<S> std::fmt::Debug for MountHandler<S> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MountHandler")
            .finish_non_exhaustive()
    }
}

impl<S: ServeSource> fofoca::iroh::protocol::ProtocolHandler for MountHandler<S> {
    async fn accept(&self, conn: Connection) -> Result<(), fofoca::iroh::protocol::AcceptError> {
        let source = (*self.source).clone();
        let auth = self.auth;
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(error) = serve_mount(conn, auth, source).await {
                web_sys::console::debug_1(&error);
            }
        });
        Ok(())
    }
}

#[derive(Clone)]
pub(crate) struct SignalHandler {
    local: fofoca::iroh::EndpointId,
    hub: send_wrapper::SendWrapper<Arc<BrowserHubTransport>>,
}

impl SignalHandler {
    pub(crate) fn new(local: fofoca::iroh::EndpointId, hub: Arc<BrowserHubTransport>) -> Self {
        Self {
            local,
            hub: send_wrapper::SendWrapper::new(hub),
        }
    }
}

impl std::fmt::Debug for SignalHandler {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SignalHandler")
            .finish_non_exhaustive()
    }
}

impl fofoca::iroh::protocol::ProtocolHandler for SignalHandler {
    async fn accept(&self, conn: Connection) -> Result<(), fofoca::iroh::protocol::AcceptError> {
        let local = self.local;
        let hub = Arc::clone(&*self.hub);
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(error) = serve_signal(&conn, local, &hub).await {
                web_sys::console::debug_1(&error);
            }
        });
        Ok(())
    }
}

/// Synthetic bench producer: answers [`OP_BENCH`] only (no real files).
#[wasm_bindgen]
pub struct BenchProducer {
    ticket: String,
    stop_tx: RefCell<Option<oneshot::Sender<()>>>,
    stopped: Cell<bool>,
    endpoint: Endpoint,
    hub: Arc<BrowserHubTransport>,
}

#[wasm_bindgen]
impl BenchProducer {
    /// Mint a ticket for `transport` (`webrtc` | `relay`) and serve echo/fill
    /// until [`BenchProducer::stop`].
    ///
    /// # Errors
    /// Unknown transport, bind failure, or no reachable relay.
    pub async fn start(transport: String) -> Result<BenchProducer, JsValue> {
        console_error_panic_hook::set_once();
        let mode = transport.trim().to_ascii_lowercase();
        let (kind, with_webrtc) = match mode.as_str() {
            "webrtc" | "webrtc_only" | "webrtc-only" => (TICKET_KIND_BENCH_WEBRTC, true),
            "relay" | "relay_only" | "relay-only" | "iroh_relay" | "iroh-relay" => {
                (TICKET_KIND_BENCH_RELAY, false)
            }
            other => {
                return Err(JsValue::from_str(&format!(
                    "unknown transport {other:?}; expected webrtc or relay"
                )));
            }
        };

        let key = SecretKey::generate();
        let local = key.public();
        let hub = BrowserHubTransport::new(local);
        let handle = WebRtcHandle::new(Arc::clone(&hub));

        // Bound before the endpoint for the reason `ShareProducer::start` gives:
        // the ticket below advertises this ladder, so the endpoint must home on
        // it too.
        let lookups = LookupOpts::public_preset();

        let mut builder = Endpoint::builder(presets::Minimal)
            .secret_key(key)
            .relay_mode(crate::relay_mode(&lookups.relay));
        builder = if with_webrtc {
            builder
                .alpns(vec![MOUNT_ALPN.to_vec(), WEBRTC_SIGNAL_ALPN.to_vec()])
                .add_custom_transport(handle.transport())
                .path_selector(handle.path_selector())
        } else {
            // Browser endpoints have no IP transports; relay-only ALPN is enough.
            builder.alpns(vec![MOUNT_ALPN.to_vec()])
        };
        let endpoint = builder
            .bind()
            .await
            .map_err(|error| err("bind bench producer endpoint", &error))?;

        let mut secret = [0u8; SECRET_LEN];
        getrandom::fill(&mut secret).map_err(|error| err("mint secret", &error))?;

        let (stop_tx, stop_rx) = oneshot::channel::<()>();
        let accept_endpoint = endpoint.clone();
        let accept_hub = Arc::clone(&hub);
        let accept_webrtc = with_webrtc;
        wasm_bindgen_futures::spawn_local(async move {
            accept_bench_loop(accept_endpoint, accept_hub, secret, accept_webrtc, stop_rx).await;
        });

        wait_until_dialable(&endpoint).await?;
        let ticket = MountTicket {
            addr: endpoint.addr(),
            secret,
            lookups,
            kind,
            mesh_id: None,
            author: None,
            // A bench share is synthetic — no directory, no bytes, nothing
            // worth protecting — so it never carries a password.
            flags: 0,
        };

        Ok(BenchProducer {
            ticket: ticket.encode(),
            stop_tx: RefCell::new(Some(stop_tx)),
            stopped: Cell::new(false),
            endpoint,
            hub,
        })
    }

    #[wasm_bindgen(getter)]
    pub fn ticket(&self) -> String {
        self.ticket.clone()
    }

    /// Stop accepting peers. Idempotent — `&self` for the same reason
    /// [`ShareProducer::stop`] is; see the note there.
    pub async fn stop(&self) -> Result<(), JsValue> {
        if self.stopped.replace(true) {
            return Ok(());
        }
        if let Some(tx) = self.stop_tx.borrow_mut().take() {
            let _ = tx.send(());
        }
        // Close live WebRTC sessions before the endpoint: dropping them clears
        // their browser handlers and closes the peer connections.
        self.hub.detach_all();
        self.endpoint.close().await;
        Ok(())
    }
}

async fn accept_bench_loop(
    endpoint: Endpoint,
    hub: Arc<BrowserHubTransport>,
    secret: [u8; SECRET_LEN],
    with_webrtc: bool,
    mut stop_rx: oneshot::Receiver<()>,
) {
    loop {
        let incoming = futures::future::select(Box::pin(endpoint.accept()), &mut stop_rx).await;
        match incoming {
            futures::future::Either::Right((_, _)) => break,
            futures::future::Either::Left((None, _)) => break,
            futures::future::Either::Left((Some(incoming), _)) => {
                let hub = Arc::clone(&hub);
                let local = endpoint.id();
                wasm_bindgen_futures::spawn_local(async move {
                    if let Err(error) =
                        accept_bench_one(incoming, local, &hub, secret, with_webrtc).await
                    {
                        web_sys::console::error_1(&error);
                    }
                });
            }
        }
    }
}

async fn accept_bench_one(
    incoming: fofoca::iroh::endpoint::Incoming,
    local: fofoca::iroh::EndpointId,
    hub: &BrowserHubTransport,
    secret: [u8; SECRET_LEN],
    with_webrtc: bool,
) -> Result<(), JsValue> {
    let conn = incoming
        .await
        .map_err(|error| err("incoming connection", &error))?;
    if conn.alpn() == WEBRTC_SIGNAL_ALPN {
        if !with_webrtc {
            return Err(JsValue::from_str(
                "unexpected WebRTC signal on a relay-only bench producer",
            ));
        }
        return serve_signal(&conn, local, hub).await;
    }
    serve_bench(conn, secret).await
}

async fn serve_bench(conn: Connection, secret: [u8; SECRET_LEN]) -> Result<(), JsValue> {
    while let Ok((send, recv)) = conn.accept_bi().await {
        let conn = conn.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let _ = serve_bench_stream(&conn, send, recv, &secret).await;
        });
    }
    Ok(())
}

async fn serve_bench_stream(
    conn: &Connection,
    mut send: fofoca::iroh::endpoint::SendStream,
    mut recv: fofoca::iroh::endpoint::RecvStream,
    secret: &[u8; SECRET_LEN],
) -> Result<(), JsValue> {
    let mut header = [0u8; REQUEST_HEADER_LEN];
    if recv.read_exact(&mut header).await.is_err() {
        return Ok(());
    }
    if &header[..SECRET_LEN] != secret {
        conn.close(1u32.into(), b"bad secret");
        return Ok(());
    }
    if header[SECRET_LEN] != OP_BENCH {
        return Ok(());
    }
    let mut prefix = [0u8; 5];
    if recv.read_exact(&mut prefix).await.is_err() {
        return Ok(());
    }
    let (kind, len) =
        decode_bench_request_prefix(&prefix).map_err(|error| err("bench prefix", &error))?;
    match kind {
        BENCH_KIND_ECHO => {
            if len == 0 || len > MAX_BENCH_ECHO_BYTES {
                return Ok(());
            }
            let mut payload = vec![0u8; len as usize];
            if recv.read_exact(&mut payload).await.is_err() {
                return Ok(());
            }
            send.write_all(&[ReadStatus::Ok.to_byte()])
                .await
                .map_err(|error| err("write status", &error))?;
            send.write_all(&len.to_le_bytes())
                .await
                .map_err(|error| err("write len", &error))?;
            send.write_all(&payload)
                .await
                .map_err(|error| err("write echo", &error))?;
        }
        BENCH_KIND_FILL => {
            if len == 0 || len > MAX_BENCH_FILL_BYTES {
                return Ok(());
            }
            send.write_all(&[ReadStatus::Ok.to_byte()])
                .await
                .map_err(|error| err("write status", &error))?;
            send.write_all(&len.to_le_bytes())
                .await
                .map_err(|error| err("write len", &error))?;
            let mut chunk = vec![0u8; 16 * 1024];
            for (index, byte) in chunk.iter_mut().enumerate() {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "index % 251 always fits in u8"
                )]
                {
                    *byte = (index % 251) as u8;
                }
            }
            let mut left = len as usize;
            while left > 0 {
                let take = left.min(chunk.len());
                send.write_all(&chunk[..take])
                    .await
                    .map_err(|error| err("write fill", &error))?;
                left -= take;
            }
        }
        OP_CHUNK_MAP | OP_CHUNK | OP_HAVE => {
            // A bench producer serves synthetic traffic and no content at all,
            // so it can address nothing — but it must *say* so rather than drop
            // the stream. A vanished stream reads as a broken connection on the
            // far side; `BadIndex` reads as "I cannot answer for that", which
            // every caller already handles.
            send.write_all(&[ReadStatus::BadIndex.to_byte()])
                .await
                .map_err(|error| err("write chunk status", &error))?;
        }
        _ => return Ok(()),
    }
    let _ = send.finish();
    Ok(())
}

struct Scanned {
    dirs: Vec<DirEntry>,
    files: Vec<(FileEntry, FileSystemFileHandle)>,
}

fn parse_listing(listing: &JsValue) -> Result<Scanned, JsValue> {
    let dirs_val = Reflect::get(listing, &JsValue::from_str("dirs"))
        .map_err(|error| js_err("listing.dirs", error))?;
    let files_val = Reflect::get(listing, &JsValue::from_str("files"))
        .map_err(|error| js_err("listing.files", error))?;
    let dirs_arr: Array = dirs_val
        .dyn_into()
        .map_err(|_| JsValue::from_str("listing.dirs must be an array"))?;
    let files_arr: Array = files_val
        .dyn_into()
        .map_err(|_| JsValue::from_str("listing.files must be an array"))?;

    let mut dirs = Vec::new();
    for i in 0..dirs_arr.length() {
        let path = dirs_arr
            .get(i)
            .as_string()
            .ok_or_else(|| JsValue::from_str("dir path must be a string"))?;
        if !safe_rel_path(&path) {
            continue;
        }
        dirs.push(DirEntry {
            rel_path: path,
            mode: 0o755,
            mtime: 0,
        });
    }

    let mut files = Vec::new();
    for i in 0..files_arr.length() {
        let entry = files_arr.get(i);
        let rel_path = Reflect::get(&entry, &JsValue::from_str("rel_path"))
            .ok()
            .and_then(|v| v.as_string())
            .ok_or_else(|| JsValue::from_str("file.rel_path missing"))?;
        if !safe_rel_path(&rel_path) {
            continue;
        }
        let size = Reflect::get(&entry, &JsValue::from_str("size"))
            .ok()
            .and_then(|v| v.as_f64())
            .ok_or_else(|| JsValue::from_str("file.size missing"))? as u64;
        let mtime = Reflect::get(&entry, &JsValue::from_str("mtime"))
            .ok()
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0) as i64;
        let handle_val = Reflect::get(&entry, &JsValue::from_str("handle"))
            .map_err(|error| js_err("file.handle", error))?;
        let handle: FileSystemFileHandle = handle_val
            .dyn_into()
            .map_err(|_| JsValue::from_str("file.handle must be a FileSystemFileHandle"))?;
        files.push((
            FileEntry {
                rel_path,
                size,
                mode: 0o644,
                mtime,
            },
            handle,
        ));
    }
    Ok(Scanned { dirs, files })
}

fn safe_rel_path(path: &str) -> bool {
    if path.is_empty() || path.starts_with('/') || path.contains('\\') || path.contains('\0') {
        return false;
    }
    path.split('/')
        .all(|part| !part.is_empty() && part != "." && part != "..")
}
async fn serve_signal(
    conn: &Connection,
    local: fofoca::iroh::EndpointId,
    hub: &BrowserHubTransport,
) -> Result<(), JsValue> {
    let remote = conn.remote_id();
    let (mut send, mut recv) = conn
        .accept_bi()
        .await
        .map_err(|error| err("accept signal stream", &error))?;
    let raw = recv
        .read_to_end(MAX_ENVELOPE_BYTES)
        .await
        .map_err(|error| err("read signal offer", &error))?;

    // The mirror of the native producer's refusal. This tab's mount lane and
    // its mesh lane share one hub, so either can reach a peer first, and the
    // registry refuses the second session. Say so before paying for a TURN
    // credential fetch and a full ICE gather that would end in that refusal.
    if hub.has_session(&remote) {
        let encoded = serde_json::to_vec(&SignalEnvelope::error(
            "a WebRTC session with you already exists; dial the custom addr",
        ))
        .map_err(|error| err("encode refusal", &error))?;
        send.write_all(&encoded)
            .await
            .map_err(|error| err("send refusal", &error))?;
        send.finish()
            .map_err(|error| err("finish signal", &error))?;
        // Returning closes the connection, so wait for the acknowledgement
        // first or the offerer reads a connection error instead of the reason.
        // The answer path below is only safe without this because it then
        // spends seconds inside `complete()`.
        let _ = send.stopped().await;
        return Ok(());
    }

    let offer: SignalEnvelope =
        serde_json::from_slice(&raw).map_err(|error| err("parse signal offer", &error))?;

    // STUN only — see the note at the consumer's `negotiate`.
    let ice = IceServers::default();
    let (pending, answer) = browser_answer(local, &offer, &ice).await?;
    let encoded = serde_json::to_vec(&answer).map_err(|error| err("encode answer", &error))?;
    send.write_all(&encoded)
        .await
        .map_err(|error| err("send answer", &error))?;
    send.finish()
        .map_err(|error| err("finish signal", &error))?;
    if let Err(error) = pending.complete(hub, remote).await {
        log_signal_sdps("producer", &answer, &offer);
        return Err(error);
    }
    Ok(())
}

async fn serve_mount<S: ServeSource>(
    conn: Connection,
    auth: ShareAuth,
    source: S,
) -> Result<(), JsValue> {
    while let Ok((send, recv)) = conn.accept_bi().await {
        let conn = conn.clone();
        let source = source.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let _ = serve_stream(&conn, send, recv, &auth, source).await;
        });
    }
    Ok(())
}

async fn answer_read(shared: &Shared, index: u32, offset: u64, len: u32) -> (ReadStatus, Vec<u8>) {
    if len > MAX_READ_LEN {
        return (ReadStatus::LenOverCap, Vec::new());
    }

    let handle = {
        let borrowed = shared.borrow();
        match borrowed.state.slot(index) {
            Some(handle) => handle.clone(),
            None => return (ReadStatus::BadIndex, Vec::new()),
        }
    };

    match read_from_handle(&handle, offset, len).await {
        Ok(bytes) => (ReadStatus::Ok, bytes),
        Err(()) => {
            // An update may have installed a fresh handle; retry once.
            let handle = {
                let borrowed = shared.borrow();
                match borrowed.state.slot(index) {
                    Some(handle) => handle.clone(),
                    None => return (ReadStatus::BadIndex, Vec::new()),
                }
            };
            match read_from_handle(&handle, offset, len).await {
                Ok(bytes) => (ReadStatus::Ok, bytes),
                Err(()) => (ReadStatus::Io, Vec::new()),
            }
        }
    }
}

async fn read_from_handle(
    handle: &FileSystemFileHandle,
    offset: u64,
    len: u32,
) -> Result<Vec<u8>, ()> {
    let file = JsFuture::from(handle.get_file()).await.map_err(|_| ())?;
    let file: web_sys::File = file.dyn_into().map_err(|_| ())?;
    let live = file.size() as u64;
    let want = (len as u64).min(live.saturating_sub(offset));
    if want == 0 {
        return Ok(Vec::new());
    }
    let end = offset + want;
    let blob = file
        .slice_with_f64_and_f64(offset as f64, end as f64)
        .map_err(|_| ())?;
    let buffer = JsFuture::from(blob.array_buffer()).await.map_err(|_| ())?;
    Ok(Uint8Array::new(&buffer).to_vec())
}

fn err(context: &str, error: &impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&format!("{context}: {error}"))
}

fn js_err(context: &str, error: JsValue) -> JsValue {
    JsValue::from_str(&format!("{context}: {error:?}"))
}

async fn wait_ms(millis: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        if let Some(window) = web_sys::window() {
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, millis);
        }
    });
    let _ = JsFuture::from(promise).await;
}

/// Wait until the endpoint has a relay URL peers can dial (≤8s, like native).
async fn wait_until_dialable(endpoint: &Endpoint) -> Result<(), JsValue> {
    let _ = futures::future::select(Box::pin(endpoint.online()), Box::pin(wait_ms(8_000))).await;
    if endpoint.addr().relay_urls().next().is_none() {
        return Err(JsValue::from_str(
            "could not reach an iroh relay; check the network and try again",
        ));
    }
    Ok(())
}
