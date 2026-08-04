use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use agent_share_proto::framing::decode_response_header;
use agent_share_proto::manifest::ManifestDelta;
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use iroh::Endpoint;
use iroh::endpoint::{Connection, RecvStream, SendStream};
use nfsserve::tcp::{NFSTcp, NFSTcpListener};
use tokio::sync::Mutex;

use crate::file::wire::read_u32;
use crate::lookup::{add_peer_addr, build_endpoint};

use super::MountTicket;
use super::mesh::ShareMesh;
use super::nfs;
use super::nfs::{ByteSource, RemoteFs, TreeIds, build_tree};
use super::{
    MAX_MANIFEST_BYTES, MAX_OUTBOARD_BYTES, MOUNT_ALPN, OP_HASH, OP_MANIFEST, OP_READ, OP_WATCH,
    SECRET_LEN,
};
// The root type comes from the store, not from this crate: `agent-share` names
// what `fofoca-blobs` verifies against rather than defining a second one.
use super::{MountManifest, ReadStatus};
use super::{WATCH_FRAME_DELTA, WATCH_FRAME_MANIFEST};
use fofoca_blobs::Root;
use fofoca_iroh_webrtc_transport::{IceConfig, WebRtcHandle};

/// How long to keep retrying the dial while the producer's address propagates
/// (mDNS is instant on a LAN; the DHT fallback can take tens of seconds).
const DISCOVERY_DEADLINE: Duration = Duration::from_secs(90);
const RETRY_DELAY: Duration = Duration::from_secs(3);

/// Consumer: redeem `ticket`, expose the remote tree through a loopback `NFSv3`
/// bridge, and mount it under `target` (read-only). Creates
/// `agent-share-YYYY-MM-DDTHHMM/` inside `target`, mounts there, and on Ctrl-C
/// unmounts and removes that empty folder. With `no_mount` (or when the OS
/// mount fails) the bridge stays up and the exact mount command is printed to
/// run manually.
///
/// # Errors
/// A malformed ticket, an unreachable producer, a hostile manifest, a bad
/// target directory, or the NFS bridge failing to bind.
pub(crate) async fn attach(
    ticket: &str,
    target: &Path,
    no_mount: bool,
    json: bool,
    webrtc_only: bool,
) -> Result<()> {
    let ticket = MountTicket::decode(ticket)?;
    // Pin a key so the WebRTC transport advertises the identity this endpoint
    // binds — the producer does the same, for the same reason.
    let mut key_bytes = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rng(), &mut key_bytes);
    let key = iroh::SecretKey::from_bytes(&key_bytes);
    let webrtc = WebRtcHandle::new(fofoca_iroh_webrtc_transport::WebRtcTransport::new(
        key.public(),
    ));
    // `clear_ip` under `--transport webrtc`, for the same reason the bench's
    // WebRTC arm sets it: a lane is pinned by removing the alternatives, not by
    // hoping the preferred one wins a race. The address book is seeded with the
    // producer's IP and relay below (the JSEP dial needs them), iroh fans the
    // mount Initial across everything it knows, and on one host the direct IP
    // path answers first — measured, the assertion reports
    // `paths=["*ip", "relay"]`. The WebRTC lane keeps its own UDP socket, so
    // ICE is unaffected; the relay stays for rendezvous.
    let endpoint = build_endpoint(
        &ticket.lookups,
        Some(key),
        None,
        Vec::new(),
        Some(webrtc.clone()),
        webrtc_only,
    )
    .await?;
    add_peer_addr(&endpoint, ticket.addr.clone())?;
    // Taken before the ticket is moved into the client, and off the ticket
    // rather than off the local bindings above: the mesh every peer of this
    // share derives is the one the ticket describes.
    let secret = ticket.secret;
    let lookups = ticket.lookups.clone();
    let client = RemoteClient::new(endpoint.clone(), ticket)
        .with_webrtc(webrtc.clone())
        .webrtc_only(webrtc_only);

    let client = Arc::new(client);
    let manifest = client.fetch_manifest().await?;
    let file_count = manifest.files.len();
    // Taken before `manifest` moves into the watch task below. Re-encodes
    // rather than hashing the wire bytes, which `fetch_manifest` discards;
    // safe because the encoding is canonical (`encoding_is_canonical`).
    let tree_fingerprint = manifest.fingerprint();
    let mut ids = TreeIds::default();
    let nodes = build_tree(&mut ids, &manifest)?;

    let mountpoint = prepare_mount_dir(target)?;
    let (uid, gid) = mountpoint_owner(&mountpoint)?;
    let remote_fs = RemoteFs::new(nodes, Arc::clone(&client), uid, gid);
    // Taken before the server consumes the filesystem: this is the watch
    // task's only way back to the tree.
    let shared_nodes = remote_fs.nodes();
    tokio::spawn(watch_tree(Arc::clone(&client), shared_nodes, ids, manifest));

    let listener = NFSTcpListener::bind("127.0.0.1:0", remote_fs)
        .await
        .context("binding the loopback NFS bridge failed")?;
    let nfs_port = listener.get_listen_port();
    tokio::spawn(async move {
        if let Err(error) = listener.handle_forever().await {
            tracing::warn!(%error, "NFS bridge stopped");
        }
    });

    let command = mount_command(nfs_port, &mountpoint);
    let mounted = !no_mount && try_mount(&command).await;
    if mounted {
        if !json {
            crate::util::output::status_out(
                "Mounted",
                &format!(
                    "{} ({file_count} files, read-only) — Ctrl-C unmounts",
                    mountpoint.display()
                ),
            );
        }
    } else {
        // The bridge keeps serving either way; hand the user the exact
        // command (some setups need sudo for the mount step). The `Run` line
        // stays a clean copy-pastable command; the hint rides the Bridge line.
        let sudo_hint = if cfg!(target_os = "linux") {
            " — run the mount command (may need sudo)"
        } else {
            " — run the mount command"
        };
        if json {
            println!("{}", command.display);
        } else {
            crate::util::output::status_out(
                "Bridge",
                &format!("NFS ready on 127.0.0.1:{nfs_port}{sudo_hint}"),
            );
            crate::util::output::status_out("Run", &command.display);
        }
    }

    // Last, on purpose. See `join_share_mesh`.
    let share_mesh = join_share_mesh(MeshJoin {
        secret: &secret,
        lookups: &lookups,
        endpoint: &endpoint,
        webrtc: &webrtc,
        webrtc_only,
        json,
        tree: Some(tree_fingerprint),
    })
    .await;

    tokio::signal::ctrl_c()
        .await
        .context("waiting for Ctrl-C failed")?;
    if mounted {
        unmount(&mountpoint).await;
    }
    // Before the endpoint closes: `Left` has to go out over it, and peers that
    // never hear it wait out a silence timeout counting us as present.
    if let Some(mesh) = share_mesh {
        mesh.leave().await;
    }
    // Best-effort: leave nothing behind when the folder is empty / unused.
    let _ = std::fs::remove_dir(&mountpoint);
    endpoint.close().await;
    Ok(())
}

/// What this consumer joins the share's mesh as. Bundled because the two
/// booleans are adjacent and would otherwise be swappable in silence.
struct MeshJoin<'a> {
    secret: &'a [u8; SECRET_LEN],
    /// Fingerprint of the manifest this consumer mounted.
    tree: Option<String>,
    lookups: &'a agent_share_proto::lookup::LookupOpts,
    endpoint: &'a Endpoint,
    webrtc: &'a WebRtcHandle,
    webrtc_only: bool,
    json: bool,
}

/// Put this consumer on the share's mesh, so it is a peer of everyone else
/// holding the link rather than a client of the producer alone.
///
/// Called *after* the bridge is up and the mount reported, and that ordering is
/// the point: the mesh is additional to the mount protocol, never a
/// precondition for it. File bytes ride the ticket's address either way, so
/// nothing about standing a mesh up belongs in front of the thing the user
/// asked for — and a relay it cannot reach must cost the mount nothing.
///
/// Non-fatal by the same rule: failure warns and returns `None`, which reads as
/// "no peer counts" and never as "no mount".
async fn join_share_mesh(join: MeshJoin<'_>) -> Option<ShareMesh> {
    let result = super::mesh::join(super::mesh::JoinOpts {
        secret: join.secret,
        lookups: join.lookups,
        shared: agent_habilis_mesh::runtime::InjectedEndpoint {
            endpoint: join.endpoint.clone(),
            webrtc: join.webrtc.clone(),
        },
        // A consumer answers no ALPN of its own — it dials the mount protocol,
        // it does not serve it — so the mesh's Router is the only accept loop
        // on this endpoint and everything it accepts belongs to the mesh.
        protocols: Vec::new(),
        role: super::mesh::Role::Consumer,
        tree: join.tree,
        // A lazy mount holds no bytes, so it advertises nothing. Becoming a
        // seeder is the explicit `mirror` step, never a side effect of reading.
        serving: None,
        // Match the endpoint: `--transport webrtc` built it with IP cleared,
        // and a mesh advertising paths its endpoint does not have is a mesh
        // whose peers dial nowhere.
        transports: if join.webrtc_only {
            agent_habilis_mesh::net::TransportOpts::webrtc_only()
        } else {
            agent_habilis_mesh::net::TransportOpts::default()
        },
    })
    .await;
    match result {
        Ok(mesh) => {
            tracing::info!(mesh = mesh.mesh_id(), "joined the share mesh");
            mesh.spawn_report(join.json);
            Some(mesh)
        }
        Err(error) => {
            tracing::warn!(%error, "share mesh unavailable; mounting without peer discovery");
            None
        }
    }
}

/// Keep the mounted tree in step with the producer's, for as long as the
/// mount lives.
///
/// Rebuilds the whole inode table per frame rather than patching it. The
/// rebuild is cheap next to a network round-trip and, more to the point, it
/// reuses exactly one code path — a second, incremental tree builder is a
/// second chance to disagree with the first about what the share holds. What
/// must survive the rebuild is fileids, and [`TreeIds`] carries those across.
///
/// Never fatal: a producer too old to know [`OP_WATCH`] drops the stream, and
/// the mount simply stays on the tree it already has.
async fn watch_tree(
    client: Arc<RemoteClient>,
    nodes: nfs::SharedNodes,
    mut ids: TreeIds,
    mut manifest: MountManifest,
) {
    loop {
        match follow_watch_stream(&client, &nodes, &mut ids, &mut manifest).await {
            Ok(()) => {
                tracing::debug!("the producer closed the watch stream");
                return;
            }
            Err(error) => {
                tracing::debug!(%error, "watch stream ended; retrying");
                tokio::time::sleep(RETRY_DELAY).await;
            }
        }
    }
}

/// Read watch frames until the stream ends, applying each to the tree.
async fn follow_watch_stream(
    client: &RemoteClient,
    nodes: &nfs::SharedNodes,
    ids: &mut TreeIds,
    manifest: &mut MountManifest,
) -> Result<()> {
    let (mut send, mut recv) = client.request(OP_WATCH).await?;
    // Nothing more to say on this stream; the producer answers until it or we
    // go away.
    send.finish().ok();
    loop {
        let mut prefix = [0u8; 5];
        if recv.read_exact(&mut prefix).await.is_err() {
            // A clean end: either the producer is done, or it predates
            // OP_WATCH and dropped the stream. Both mean "no more updates".
            return Ok(());
        }
        let len = decode_response_header(&prefix, MAX_MANIFEST_BYTES)?;
        let mut body = vec![0u8; usize::try_from(len).expect("u32 fits usize")];
        recv.read_exact(&mut body)
            .await
            .context("reading a watch frame")?;
        let (kind, payload) = body.split_first().context("empty watch frame")?;
        match *kind {
            WATCH_FRAME_MANIFEST => *manifest = MountManifest::decode(payload)?,
            WATCH_FRAME_DELTA => manifest.apply(&ManifestDelta::decode(payload)?),
            other => bail!("unknown watch frame kind: {other}"),
        }
        // A hostile manifest fails the build; keep the tree we had rather than
        // tearing the mount down over one bad frame.
        match build_tree(ids, manifest) {
            Ok(rebuilt) => {
                tracing::debug!(files = manifest.files.len(), "tree updated");
                nfs::replace_nodes(nodes, rebuilt);
            }
            Err(error) => tracing::warn!(%error, "rejecting a bad tree update"),
        }
    }
}

/// The wire client the NFS layer reads through: one shared QUIC connection,
/// one bi-stream per request, redialed transparently if it drops.
pub(super) struct RemoteClient {
    endpoint: Endpoint,
    ticket: MountTicket,
    conn: Mutex<Option<Connection>>,
    /// The `WebRTC` lane, when this consumer registered one. `None` keeps the
    /// dial on IP/relay only, which is what the offline tests want.
    webrtc: Option<WebRtcHandle>,
    /// Refuse anything but the data channel: skip the IP/relay attempt entirely
    /// and fail loudly if the mount does not settle on `WebRTC`.
    webrtc_only: bool,
}

impl RemoteClient {
    pub(super) fn new(endpoint: Endpoint, ticket: MountTicket) -> Self {
        Self {
            endpoint,
            ticket,
            conn: Mutex::new(None),
            webrtc: None,
            webrtc_only: false,
        }
    }

    /// Register a `WebRTC` lane to fall back on.
    #[must_use]
    pub(super) fn with_webrtc(mut self, handle: WebRtcHandle) -> Self {
        self.webrtc = Some(handle);
        self
    }

    /// Make the `WebRTC` lane the *only* lane.
    ///
    /// Not a preference — a requirement. The ordinary path is skipped rather
    /// than tried first, and the selected path is asserted afterwards, so a run
    /// that claims `WebRTC` can be shown to be one. Without this the consumer
    /// spends `DISCOVERY_DEADLINE` on IP/relay before it will even look at the
    /// data channel, which is the right default for two native peers and
    /// useless for testing the lane.
    #[must_use]
    pub(super) fn webrtc_only(mut self, only: bool) -> Self {
        self.webrtc_only = only;
        self
    }

    /// Try the `WebRTC` lane: negotiate a data channel, then dial the mount
    /// ALPN over an address carrying only that channel.
    ///
    /// Tried **after** the ordinary path, not before. Two native peers are
    /// better served by iroh's own hole-punching; tunnelling QUIC inside SCTP
    /// inside DTLS stacks two congestion controllers for nothing. This earns
    /// its place on the NATs that defeat hole-punching but not ICE.
    async fn connect_over_webrtc(&self) -> Result<Connection> {
        let handle = self
            .webrtc
            .as_ref()
            .context("no WebRTC lane registered on this consumer")?;
        let addr = Box::pin(super::dial_webrtc(
            &self.endpoint,
            self.ticket.addr.clone(),
            handle,
            &IceConfig::default(),
        ))
        .await?;
        self.endpoint
            .connect(addr, MOUNT_ALPN)
            .await
            .context("dial the mount ALPN over the WebRTC data channel")
    }

    #[cfg(test)]
    pub(super) fn producer_addr(&self) -> iroh::EndpointAddr {
        self.ticket.addr.clone()
    }

    /// The shared connection, dialing (with the discovery retry loop) when
    /// there is none or the previous one died.
    async fn connection(&self) -> Result<Connection> {
        let mut guard = self.conn.lock().await;
        if let Some(conn) = guard.as_ref()
            && conn.close_reason().is_none()
        {
            return Ok(conn.clone());
        }
        if self.webrtc_only {
            let conn = Box::pin(self.connect_over_webrtc())
                .await
                .context("webrtc-only mount: the data channel could not be established")?;
            super::webrtc::ensure_webrtc_selected(&conn, "webrtc-only mount").await?;
            *guard = Some(conn.clone());
            return Ok(conn);
        }
        let start = Instant::now();
        let conn = loop {
            match self
                .endpoint
                .connect(self.ticket.addr.clone(), MOUNT_ALPN)
                .await
            {
                Ok(conn) => break conn,
                Err(error) if start.elapsed() < DISCOVERY_DEADLINE => {
                    tracing::warn!(%error, "connect failed; retrying");
                    tokio::time::sleep(RETRY_DELAY).await;
                }
                Err(error) => {
                    // Out of retries on the ordinary path. If a WebRTC lane is
                    // registered, negotiate one before giving up — that is the
                    // whole point of carrying it.
                    if self.webrtc.is_some() {
                        tracing::debug!(%error, "IP/relay dial failed; trying the WebRTC lane");
                        match Box::pin(self.connect_over_webrtc()).await {
                            Ok(conn) => break conn,
                            Err(webrtc_error) => {
                                return Err(anyhow::anyhow!(
                                    "could not reach the mount producer over IP/relay ({error}) \
                                     or WebRTC ({webrtc_error})"
                                ));
                            }
                        }
                    }
                    return Err(anyhow::anyhow!(
                        "could not reach the mount producer: {error}"
                    ));
                }
            }
        };
        *guard = Some(conn.clone());
        Ok(conn)
    }

    /// Open one request stream and write the `secret ‖ op` header. Retries
    /// once on a fresh connection when the cached one just died.
    async fn request(&self, op: u8) -> Result<(SendStream, RecvStream)> {
        for attempt in 0..2 {
            let conn = self.connection().await?;
            match conn.open_bi().await {
                Ok((mut send, recv)) => {
                    send.write_all(&self.ticket.secret).await?;
                    send.write_all(&[op]).await?;
                    return Ok((send, recv));
                }
                Err(error) => {
                    // Drop the dead connection; the next loop iteration
                    // redials.
                    *self.conn.lock().await = None;
                    if attempt == 1 {
                        return Err(anyhow::anyhow!("opening a request stream failed: {error}"));
                    }
                }
            }
        }
        unreachable!("the loop returns on success and on the second failure")
    }

    /// Ask the origin for a file's BLAKE3 root and bao outboard.
    ///
    /// `Ok(None)` means *this producer cannot vouch for that index* — no hash
    /// cache, an index out of range, or a file that changed under it. All three
    /// are ordinary and all three mean the same thing to a caller: read those
    /// bytes from the origin, which is what happens today anyway. Only a
    /// protocol failure is an error.
    ///
    /// The root is learned from the **origin**, over a channel already
    /// authenticated to the ticket's endpoint id. That is what makes it safe to
    /// take the bytes from anybody afterwards.
    pub(super) async fn fetch_hash(&self, index: u32) -> Result<Option<(Root, Vec<u8>)>> {
        let (mut send, mut recv) = self.request(OP_HASH).await?;
        send.write_all(&index.to_le_bytes()).await?;
        let _ = send.finish();

        let mut status = [0u8; 1];
        recv.read_exact(&mut status)
            .await
            .context("reading the hash status failed")?;
        match ReadStatus::from_byte(status[0])? {
            ReadStatus::Ok => {}
            // Not an error: see the note above. Named rather than wildcarded so
            // a future status has to be considered here rather than silently
            // folded into "cannot vouch".
            ReadStatus::BadIndex | ReadStatus::Io | ReadStatus::LenOverCap => return Ok(None),
        }

        let mut root = [0u8; 32];
        recv.read_exact(&mut root)
            .await
            .context("reading the root failed")?;
        let len = read_u32(&mut recv).await?;
        if len > MAX_OUTBOARD_BYTES {
            bail!("outboard too large: {len} bytes");
        }
        let mut outboard = vec![0u8; usize::try_from(len).expect("u32 fits usize")];
        recv.read_exact(&mut outboard)
            .await
            .context("reading the outboard failed")?;
        Ok(Some((root, outboard)))
    }

    /// The manifest as the origin sent it, before decoding.
    ///
    /// A mirror needs these exact bytes rather than a re-encode: it re-serves
    /// them verbatim so its indices stay the origin's, and it fingerprints them
    /// so peers on one tree agree. Decoding and re-encoding would be correct
    /// only for as long as the encoding stays canonical, and there is no reason
    /// to depend on that when the real bytes are right here.
    pub(super) async fn fetch_manifest_bytes(&self) -> Result<Vec<u8>> {
        let (mut send, mut recv) = self.request(OP_MANIFEST).await?;
        let _ = send.finish();
        let mut status = [0u8; 1];
        recv.read_exact(&mut status)
            .await
            .context("reading the manifest status failed")?;
        if ReadStatus::from_byte(status[0])? != ReadStatus::Ok {
            bail!("the producer refused the manifest request");
        }
        let len = read_u32(&mut recv).await?;
        if len > MAX_MANIFEST_BYTES {
            bail!("manifest too large: {len} bytes");
        }
        let mut bytes = vec![0u8; usize::try_from(len).expect("u32 fits usize")];
        recv.read_exact(&mut bytes)
            .await
            .context("reading the manifest failed")?;
        Ok(bytes)
    }

    pub(super) async fn fetch_manifest(&self) -> Result<MountManifest> {
        let (mut send, mut recv) = self.request(OP_MANIFEST).await?;
        let _ = send.finish();
        let mut status = [0u8; 1];
        recv.read_exact(&mut status)
            .await
            .context("reading the manifest status failed")?;
        if ReadStatus::from_byte(status[0])? != ReadStatus::Ok {
            bail!("the producer refused the manifest request");
        }
        let len = read_u32(&mut recv).await?;
        if len > MAX_MANIFEST_BYTES {
            bail!("manifest too large: {len} bytes");
        }
        let mut bytes = vec![0u8; usize::try_from(len).expect("u32 fits usize")];
        recv.read_exact(&mut bytes)
            .await
            .context("reading the manifest failed")?;
        MountManifest::decode(&bytes)
    }

    pub(super) async fn read_range(&self, index: u32, offset: u64, len: u32) -> Result<Vec<u8>> {
        let (mut send, mut recv) = self.request(OP_READ).await?;
        let mut request = Vec::with_capacity(16);
        request.extend_from_slice(&index.to_le_bytes());
        request.extend_from_slice(&offset.to_le_bytes());
        request.extend_from_slice(&len.to_le_bytes());
        send.write_all(&request).await?;
        let _ = send.finish();
        let mut status = [0u8; 1];
        recv.read_exact(&mut status)
            .await
            .context("reading the read status failed")?;
        match ReadStatus::from_byte(status[0])? {
            ReadStatus::Ok => {}
            ReadStatus::BadIndex => bail!("the producer does not know file index {index}"),
            ReadStatus::Io => bail!("the producer failed to read file index {index}"),
            ReadStatus::LenOverCap => bail!("read of {len} bytes exceeds the producer's cap"),
        }
        let data_len = read_u32(&mut recv).await?;
        if data_len > len {
            bail!("the producer sent more than requested");
        }
        let mut data = vec![0u8; usize::try_from(data_len).expect("u32 fits usize")];
        recv.read_exact(&mut data)
            .await
            .context("reading the file bytes failed")?;
        Ok(data)
    }
}

#[async_trait]
impl ByteSource for RemoteClient {
    async fn read(&self, index: u32, offset: u64, len: u32) -> Result<Vec<u8>> {
        self.read_range(index, offset, len).await
    }
}

/// Ensure `target` is a directory, then create `agent-share-YYYY-MM-DDTHHMM/`
/// under it (local clock, minute precision). On name collision, retry with
/// seconds, then numeric suffixes.
fn prepare_mount_dir(target: &Path) -> Result<std::path::PathBuf> {
    if target.exists() {
        if !target.is_dir() {
            bail!("mount target {} is not a directory", target.display());
        }
    } else {
        std::fs::create_dir_all(target)
            .with_context(|| format!("creating {}", target.display()))?;
    }
    let now = chrono::Local::now();
    for name in mount_folder_candidates(now) {
        let path = target.join(&name);
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(error).with_context(|| format!("creating {}", path.display()));
            }
        }
    }
    bail!(
        "could not create a unique agent-share folder under {}",
        target.display()
    )
}

/// Folder-name candidates for `now`, in collision-retry order.
fn mount_folder_candidates(now: chrono::DateTime<chrono::Local>) -> Vec<String> {
    let minute = now.format("agent-share-%Y-%m-%dT%H%M").to_string();
    let second = now.format("agent-share-%Y-%m-%dT%H%M%S").to_string();
    let mut names = vec![minute, second.clone()];
    for suffix in 2..=99 {
        names.push(format!("{second}-{suffix}"));
    }
    names
}

/// The uid/gid the served attrs report — the owner of the mountpoint, read
/// from metadata rather than `libc::getuid()` (the workspace denies `unsafe`).
#[cfg(unix)]
fn mountpoint_owner(mountpoint: &Path) -> Result<(u32, u32)> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(mountpoint)
        .with_context(|| format!("reading {}", mountpoint.display()))?;
    Ok((meta.uid(), meta.gid()))
}

#[cfg(not(unix))]
fn mountpoint_owner(_mountpoint: &Path) -> Result<(u32, u32)> {
    bail!("agent-share is only supported on macOS and Linux")
}

/// The OS mount invocation for the loopback bridge: the argv actually run
/// (the mountpoint rides as one `OsString`, never re-split) plus the
/// shell-quoted display string printed for the user. `ro` is client-side
/// enforcement on top of the server's ROFS answers; `nolocks`/`nolock`
/// (the spelling differs per OS) because the bridge serves no lock manager.
///
/// `actimeo` bounds how long the kernel trusts a cached attribute, and so how
/// stale the mount can look after the producer's tree changes. It was 120s,
/// from when the share was a startup snapshot and nothing could change under
/// it — with a watch stream running, that would have hidden every update for
/// two minutes. 10s is the compromise: attributes here are answered from the
/// in-memory tree over loopback, never from the remote peer, so revalidating
/// costs local RPCs rather than round-trips over the share's connection.
struct MountCommand {
    program: &'static str,
    args: Vec<std::ffi::OsString>,
    display: String,
}

fn mount_command(nfs_port: u16, mountpoint: &Path) -> MountCommand {
    let (program, options) = if cfg!(target_os = "macos") {
        (
            "mount_nfs",
            format!(
                "ro,nolocks,vers=3,tcp,rsize=131072,actimeo=10,port={nfs_port},mountport={nfs_port}"
            ),
        )
    } else {
        (
            "mount",
            format!(
                "ro,noacl,nolock,vers=3,tcp,rsize=131072,actimeo=10,port={nfs_port},mountport={nfs_port}"
            ),
        )
    };
    let mut args: Vec<std::ffi::OsString> = Vec::new();
    if program == "mount" {
        args.push("-t".into());
        args.push("nfs".into());
    }
    args.push("-o".into());
    args.push(options.as_str().into());
    args.push("127.0.0.1:/".into());
    args.push(mountpoint.as_os_str().to_owned());
    let flags = if program == "mount" {
        "-t nfs -o"
    } else {
        "-o"
    };
    let display = format!(
        "{program} {flags} {options} 127.0.0.1:/ {}",
        super::shell_word(&mountpoint.display().to_string())
    );
    MountCommand {
        program,
        args,
        display,
    }
}

/// Run the mount command, reporting success. Failure is not fatal — the
/// caller prints the command for the user to run (possibly with sudo).
async fn try_mount(command: &MountCommand) -> bool {
    let output = tokio::process::Command::new(command.program)
        .args(&command.args)
        .output()
        .await;
    match output {
        Ok(output) if output.status.success() => true,
        Ok(output) => {
            tracing::warn!(
                stderr = %String::from_utf8_lossy(&output.stderr).trim(),
                "mount command failed"
            );
            false
        }
        Err(error) => {
            tracing::warn!(%error, "running the mount command failed");
            false
        }
    }
}

/// Best-effort unmount on shutdown; macOS falls back to `diskutil unmount`
/// when plain `umount` is refused (e.g. Finder still holds the volume).
async fn unmount(mountpoint: &Path) {
    if run_quiet("umount", &[mountpoint.as_os_str()]).await {
        return;
    }
    if cfg!(target_os = "macos")
        && run_quiet(
            "diskutil",
            &[std::ffi::OsStr::new("unmount"), mountpoint.as_os_str()],
        )
        .await
    {
        return;
    }
    eprintln!(
        "could not unmount; run manually: umount {}",
        super::shell_word(&mountpoint.display().to_string())
    );
}

async fn run_quiet(program: &str, args: &[&std::ffi::OsStr]) -> bool {
    tokio::process::Command::new(program)
        .args(args)
        .output()
        .await
        .is_ok_and(|output| output.status.success())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn mount_folder_name_is_iso_local_minute() {
        let now = chrono::Local
            .with_ymd_and_hms(2026, 1, 1, 16, 20, 45)
            .single()
            .expect("valid local time");
        let names = mount_folder_candidates(now);
        assert_eq!(names[0], "agent-share-2026-01-01T1620");
        assert_eq!(names[1], "agent-share-2026-01-01T162045");
        assert_eq!(names[2], "agent-share-2026-01-01T162045-2");
    }

    #[test]
    fn prepare_mount_dir_creates_under_non_empty_target() {
        let target = std::env::temp_dir().join(format!(
            "agent-share-target-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&target).expect("create target");
        std::fs::write(target.join("keep.txt"), b"x").expect("occupy target");
        let mount = prepare_mount_dir(&target).expect("prepare");
        let name = mount.file_name().and_then(|os| os.to_str()).expect("utf-8");
        assert!(
            name.starts_with("agent-share-") && name.contains('T'),
            "unexpected mount folder name: {name}"
        );
        assert!(mount.is_dir());
        assert!(target.join("keep.txt").is_file());
        let _ = std::fs::remove_dir_all(&target);
    }
}
