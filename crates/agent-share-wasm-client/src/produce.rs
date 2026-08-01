//! In-browser share producer: serve a File System Access tree over WebRTC.
//!
//! Snapshot at start (no live watch). The accept loop answers the signal ALPN
//! (JSEP answerer) and the mount ALPN (manifest / read / watch snapshot).

use std::sync::Arc;

use agent_share_proto::framing::{
    BENCH_KIND_ECHO, BENCH_KIND_FILL, MAX_BENCH_ECHO_BYTES, MAX_BENCH_FILL_BYTES,
    MAX_MANIFEST_BYTES, MAX_READ_LEN, MOUNT_ALPN, OP_BENCH, OP_MANIFEST, OP_READ, OP_WATCH,
    REQUEST_HEADER_LEN, SECRET_LEN, WATCH_FRAME_MANIFEST, WEBRTC_SIGNAL_ALPN,
    decode_bench_request_prefix,
};
use agent_share_proto::lookup::LookupOpts;
use agent_share_proto::manifest::{DirEntry, FileEntry, MountManifest, ReadStatus};
use agent_share_proto::ticket::{MountTicket, TICKET_FLAG_BENCH_RELAY, TICKET_FLAG_BENCH_WEBRTC};
use fofoca_iroh_webrtc_transport::{
    BrowserHubTransport, IceServers, MAX_ENVELOPE_BYTES, SignalEnvelope, WebRtcHandle,
    browser_answer, log_signal_sdps,
};
use futures::channel::oneshot;
use iroh::endpoint::{Connection, presets};
use iroh::{Endpoint, SecretKey};
use js_sys::{Array, Reflect, Uint8Array};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::File;

/// An in-browser share, serving until [`ShareProducer::stop`].
#[wasm_bindgen]
pub struct ShareProducer {
    ticket: String,
    files: u32,
    bytes: u64,
    stop_tx: Option<oneshot::Sender<()>>,
    _endpoint: Endpoint,
    hub: Arc<BrowserHubTransport>,
}

#[wasm_bindgen]
impl ShareProducer {
    /// Start serving a pre-scanned listing from JS:
    /// `{ dirs: string[], files: { rel_path: string, size: number, file: File }[] }`.
    ///
    /// # Errors
    /// Bad listing shape, bind failure, or empty tree.
    pub async fn start(listing: JsValue) -> Result<ShareProducer, JsValue> {
        console_error_panic_hook::set_once();
        let scanned = parse_listing(&listing)?;
        if scanned.files.is_empty() && scanned.dirs.is_empty() {
            return Err(JsValue::from_str("share is empty"));
        }

        let file_count = u32::try_from(scanned.files.len()).unwrap_or(u32::MAX);
        let total_bytes: u64 = scanned.files.iter().map(|f| f.size).sum();
        let manifest = MountManifest {
            dirs: scanned
                .dirs
                .iter()
                .map(|rel_path| DirEntry {
                    rel_path: rel_path.clone(),
                    mode: 0o755,
                    mtime: 0,
                })
                .collect(),
            files: scanned
                .files
                .iter()
                .map(|f| FileEntry {
                    rel_path: f.rel_path.clone(),
                    size: f.size,
                    mode: 0o644,
                    mtime: 0,
                })
                .collect(),
        };
        let encoded = manifest.encode();
        if encoded.len() > MAX_MANIFEST_BYTES as usize {
            return Err(JsValue::from_str("tree too large to serve"));
        }

        let tree = Arc::new(ServeTree {
            manifest_bytes: encoded,
            files: scanned.files,
        });

        let key = SecretKey::generate();
        let local = key.public();
        let hub = BrowserHubTransport::new(local);
        let handle = WebRtcHandle::new(Arc::clone(&hub));

        let endpoint = Endpoint::builder(presets::Minimal)
            .secret_key(key)
            .relay_mode(iroh::endpoint::default_relay_mode())
            .alpns(vec![MOUNT_ALPN.to_vec(), WEBRTC_SIGNAL_ALPN.to_vec()])
            .add_custom_transport(handle.transport())
            .bind()
            .await
            .map_err(|error| err("bind producer endpoint", &error))?;

        let mut secret = [0u8; SECRET_LEN];
        getrandom::fill(&mut secret).map_err(|error| err("mint secret", &error))?;

        // Accept before the ticket is shown so a fast joiner is not raced.
        let (stop_tx, stop_rx) = oneshot::channel::<()>();
        let accept_endpoint = endpoint.clone();
        let accept_hub = Arc::clone(&hub);
        let accept_tree = Arc::clone(&tree);
        wasm_bindgen_futures::spawn_local(async move {
            accept_loop(accept_endpoint, accept_hub, secret, accept_tree, stop_rx).await;
        });

        // Ticket must carry a relay URL — the browser consumer has no mdns/dht
        // and dials the signal ALPN from `ticket.addr` alone.
        wait_until_dialable(&endpoint).await?;
        let ticket = MountTicket {
            addr: endpoint.addr(),
            secret,
            lookups: LookupOpts::public_preset(),
            flags: 0,
        };
        let ticket_str = ticket.encode();

        Ok(ShareProducer {
            ticket: ticket_str,
            files: file_count,
            bytes: total_bytes,
            stop_tx: Some(stop_tx),
            _endpoint: endpoint,
            hub,
        })
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
        self.files
    }

    #[wasm_bindgen(getter)]
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Stop accepting peers. Idempotent.
    pub async fn stop(mut self) -> Result<(), JsValue> {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
        // Close live WebRTC sessions before the endpoint: dropping them clears
        // their browser handlers and closes the peer connections.
        self.hub.detach_all();
        self._endpoint.close().await;
        Ok(())
    }
}

/// Synthetic bench producer: answers [`OP_BENCH`] only (no real files).
#[wasm_bindgen]
pub struct BenchProducer {
    ticket: String,
    stop_tx: Option<oneshot::Sender<()>>,
    _endpoint: Endpoint,
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
        let (flags, with_webrtc) = match mode.as_str() {
            "webrtc" | "webrtc_only" | "webrtc-only" => (TICKET_FLAG_BENCH_WEBRTC, true),
            "relay" | "relay_only" | "relay-only" | "iroh_relay" | "iroh-relay" => {
                (TICKET_FLAG_BENCH_RELAY, false)
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

        let mut builder = Endpoint::builder(presets::Minimal)
            .secret_key(key)
            .relay_mode(iroh::endpoint::default_relay_mode());
        builder = if with_webrtc {
            builder
                .alpns(vec![MOUNT_ALPN.to_vec(), WEBRTC_SIGNAL_ALPN.to_vec()])
                .add_custom_transport(handle.transport())
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
            lookups: LookupOpts::public_preset(),
            flags,
        };

        Ok(BenchProducer {
            ticket: ticket.encode(),
            stop_tx: Some(stop_tx),
            _endpoint: endpoint,
            hub,
        })
    }

    #[wasm_bindgen(getter)]
    pub fn ticket(&self) -> String {
        self.ticket.clone()
    }

    /// Stop accepting peers. Idempotent.
    pub async fn stop(mut self) -> Result<(), JsValue> {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
        // Close live WebRTC sessions before the endpoint: dropping them clears
        // their browser handlers and closes the peer connections.
        self.hub.detach_all();
        self._endpoint.close().await;
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
    incoming: iroh::endpoint::Incoming,
    local: iroh::EndpointId,
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
    mut send: iroh::endpoint::SendStream,
    mut recv: iroh::endpoint::RecvStream,
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
        _ => return Ok(()),
    }
    let _ = send.finish();
    Ok(())
}

struct ServeFile {
    rel_path: String,
    size: u64,
    file: File,
}

struct Scanned {
    dirs: Vec<String>,
    files: Vec<ServeFile>,
}

struct ServeTree {
    manifest_bytes: Vec<u8>,
    files: Vec<ServeFile>,
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
        dirs.push(path);
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
        let file_val = Reflect::get(&entry, &JsValue::from_str("file"))
            .map_err(|error| js_err("file.file", error))?;
        let file: File = file_val
            .dyn_into()
            .map_err(|_| JsValue::from_str("file.file must be a File"))?;
        files.push(ServeFile {
            rel_path,
            size,
            file,
        });
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

async fn accept_loop(
    endpoint: Endpoint,
    hub: Arc<BrowserHubTransport>,
    secret: [u8; SECRET_LEN],
    tree: Arc<ServeTree>,
    mut stop_rx: oneshot::Receiver<()>,
) {
    loop {
        let incoming = futures::future::select(Box::pin(endpoint.accept()), &mut stop_rx).await;
        match incoming {
            futures::future::Either::Right((_, _)) => break,
            futures::future::Either::Left((None, _)) => break,
            futures::future::Either::Left((Some(incoming), _)) => {
                let hub = Arc::clone(&hub);
                let tree = Arc::clone(&tree);
                let local = endpoint.id();
                wasm_bindgen_futures::spawn_local(async move {
                    if let Err(error) = accept_one(incoming, local, &hub, secret, &tree).await {
                        web_sys::console::error_1(&error);
                    }
                });
            }
        }
    }
}

async fn accept_one(
    incoming: iroh::endpoint::Incoming,
    local: iroh::EndpointId,
    hub: &BrowserHubTransport,
    secret: [u8; SECRET_LEN],
    tree: &ServeTree,
) -> Result<(), JsValue> {
    let conn = incoming
        .await
        .map_err(|error| err("incoming connection", &error))?;
    if conn.alpn() == WEBRTC_SIGNAL_ALPN {
        return serve_signal(&conn, local, hub).await;
    }
    serve_mount(conn, secret, tree).await
}

async fn serve_signal(
    conn: &Connection,
    local: iroh::EndpointId,
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
    let offer: SignalEnvelope =
        serde_json::from_slice(&raw).map_err(|error| err("parse signal offer", &error))?;

    let ice = IceServers::with_turn_fallback().await;
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

async fn serve_mount(
    conn: Connection,
    secret: [u8; SECRET_LEN],
    tree: &ServeTree,
) -> Result<(), JsValue> {
    while let Ok((send, recv)) = conn.accept_bi().await {
        let conn = conn.clone();
        let tree_manifest = tree.manifest_bytes.clone();
        let files = tree.files.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let _ = serve_stream(&conn, send, recv, &secret, &tree_manifest, &files).await;
        });
    }
    Ok(())
}

async fn serve_stream(
    conn: &Connection,
    mut send: iroh::endpoint::SendStream,
    mut recv: iroh::endpoint::RecvStream,
    secret: &[u8; SECRET_LEN],
    manifest_bytes: &[u8],
    files: &[ServeFile],
) -> Result<(), JsValue> {
    let mut header = [0u8; REQUEST_HEADER_LEN];
    if recv.read_exact(&mut header).await.is_err() {
        return Ok(());
    }
    if &header[..SECRET_LEN] != secret {
        conn.close(1u32.into(), b"bad secret");
        return Ok(());
    }
    match header[SECRET_LEN] {
        OP_MANIFEST => {
            send.write_all(&[ReadStatus::Ok.to_byte()])
                .await
                .map_err(|error| err("write status", &error))?;
            let len = u32::try_from(manifest_bytes.len())
                .map_err(|_| JsValue::from_str("manifest too large"))?;
            send.write_all(&len.to_le_bytes())
                .await
                .map_err(|error| err("write len", &error))?;
            send.write_all(manifest_bytes)
                .await
                .map_err(|error| err("write manifest", &error))?;
        }
        OP_WATCH => {
            // MVP: one snapshot frame (same opening shape as native), then idle.
            let mut frame = Vec::with_capacity(1 + manifest_bytes.len());
            frame.push(WATCH_FRAME_MANIFEST);
            frame.extend_from_slice(manifest_bytes);
            send.write_all(&[ReadStatus::Ok.to_byte()])
                .await
                .map_err(|error| err("write status", &error))?;
            let len = u32::try_from(frame.len())
                .map_err(|_| JsValue::from_str("watch frame too large"))?;
            send.write_all(&len.to_le_bytes())
                .await
                .map_err(|error| err("write len", &error))?;
            send.write_all(&frame)
                .await
                .map_err(|error| err("write watch frame", &error))?;
            // Keep the stream open; native would push deltas here.
            std::future::pending::<()>().await;
            return Ok(());
        }
        OP_READ => {
            let mut request = [0u8; 16];
            if recv.read_exact(&mut request).await.is_err() {
                return Ok(());
            }
            let index = u32::from_le_bytes(request[..4].try_into().expect("4"));
            let offset = u64::from_le_bytes(request[4..12].try_into().expect("8"));
            let len = u32::from_le_bytes(request[12..].try_into().expect("4"));
            let (status, data) = answer_read(files, index, offset, len).await;
            send.write_all(&[status.to_byte()])
                .await
                .map_err(|error| err("write status", &error))?;
            let data_len = u32::try_from(data.len()).expect("bounded");
            send.write_all(&data_len.to_le_bytes())
                .await
                .map_err(|error| err("write len", &error))?;
            send.write_all(&data)
                .await
                .map_err(|error| err("write body", &error))?;
        }
        _ => return Ok(()),
    }
    let _ = send.finish();
    Ok(())
}

async fn answer_read(
    files: &[ServeFile],
    index: u32,
    offset: u64,
    len: u32,
) -> (ReadStatus, Vec<u8>) {
    if len > MAX_READ_LEN {
        return (ReadStatus::LenOverCap, Vec::new());
    }
    let Some(file) = files.get(index as usize) else {
        return (ReadStatus::BadIndex, Vec::new());
    };
    let live = file.file.size() as u64;
    let want = (len as u64).min(live.saturating_sub(offset));
    if want == 0 {
        return (ReadStatus::Ok, Vec::new());
    }
    let end = offset + want;
    let blob = file.file.slice_with_f64_and_f64(offset as f64, end as f64);
    let Ok(blob) = blob else {
        return (ReadStatus::Io, Vec::new());
    };
    let Ok(buffer) = JsFuture::from(blob.array_buffer()).await else {
        return (ReadStatus::Io, Vec::new());
    };
    let bytes = Uint8Array::new(&buffer).to_vec();
    (ReadStatus::Ok, bytes)
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

// ServeFile needs Clone for spawn_local copies — File is cloneable via clone().
impl Clone for ServeFile {
    fn clone(&self) -> Self {
        Self {
            rel_path: self.rel_path.clone(),
            size: self.size,
            file: self.file.clone(),
        }
    }
}
