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
use super::nfs;
use super::nfs::{ByteSource, RemoteFs, TreeIds, build_tree};
use super::{MAX_MANIFEST_BYTES, MOUNT_ALPN, OP_MANIFEST, OP_READ, OP_WATCH};
use super::{MountManifest, ReadStatus};
use super::{WATCH_FRAME_DELTA, WATCH_FRAME_MANIFEST};
use fofoca_iroh_webrtc_transport::{IceConfig, WebRtcHandle};

/// How long to keep retrying the dial while the producer's address propagates
/// (mDNS is instant on a LAN; the DHT fallback can take tens of seconds).
const DISCOVERY_DEADLINE: Duration = Duration::from_secs(90);
const RETRY_DELAY: Duration = Duration::from_secs(3);

/// Consumer: redeem `ticket`, expose the remote tree through a loopback `NFSv3`
/// bridge, and mount it at `mountpoint` (read-only). Serves until Ctrl-C,
/// then unmounts. With `no_mount` (or when the OS mount fails) the bridge
/// stays up and the exact mount command is printed to run manually.
///
/// # Errors
/// A malformed ticket, an unreachable producer, a hostile manifest, a
/// non-empty mountpoint, or the NFS bridge failing to bind.
pub(crate) async fn attach(
    ticket: &str,
    mountpoint: &Path,
    no_mount: bool,
    json: bool,
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
    let endpoint = build_endpoint(
        &ticket.lookups,
        Some(key),
        None,
        Vec::new(),
        Some(webrtc.clone()),
        false,
    )
    .await?;
    add_peer_addr(&endpoint, ticket.addr.clone())?;
    let client = RemoteClient::new(endpoint.clone(), ticket).with_webrtc(webrtc);

    let client = Arc::new(client);
    let manifest = client.fetch_manifest().await?;
    let file_count = manifest.files.len();
    let mut ids = TreeIds::default();
    let nodes = build_tree(&mut ids, &manifest)?;

    prepare_mountpoint(mountpoint)?;
    let (uid, gid) = mountpoint_owner(mountpoint)?;
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

    let command = mount_command(nfs_port, mountpoint);
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

    tokio::signal::ctrl_c()
        .await
        .context("waiting for Ctrl-C failed")?;
    if mounted {
        unmount(mountpoint).await;
    }
    endpoint.close().await;
    Ok(())
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
}

impl RemoteClient {
    pub(super) fn new(endpoint: Endpoint, ticket: MountTicket) -> Self {
        Self {
            endpoint,
            ticket,
            conn: Mutex::new(None),
            webrtc: None,
        }
    }

    /// Register a `WebRTC` lane to fall back on.
    #[must_use]
    pub(super) fn with_webrtc(mut self, handle: WebRtcHandle) -> Self {
        self.webrtc = Some(handle);
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

/// Create the mountpoint if missing; an existing one must be an empty
/// directory (never removed on exit).
fn prepare_mountpoint(mountpoint: &Path) -> Result<()> {
    if mountpoint.exists() {
        if !mountpoint.is_dir() {
            bail!("mountpoint {} is not a directory", mountpoint.display());
        }
        let occupied = mountpoint
            .read_dir()
            .with_context(|| format!("reading {}", mountpoint.display()))?
            .next()
            .is_some();
        if occupied {
            bail!("mountpoint {} is not empty", mountpoint.display());
        }
        return Ok(());
    }
    std::fs::create_dir_all(mountpoint)
        .with_context(|| format!("creating {}", mountpoint.display()))
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
