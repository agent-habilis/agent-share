use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use iroh::endpoint::{Connection, Incoming, RecvStream, SendStream};
use iroh::{Endpoint, EndpointId, SecretKey};
use rand::RngCore;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::broadcast;

use crate::file::human_bytes;
use crate::lookup::build_endpoint;
use crate::protocol::swarm::{LookupOpts, LookupSet, resolve_transfer_lookups};

use super::MountTicket;
use super::ReadStatus;
use super::live::LiveTree;
use super::{
    MAX_READ_LEN, MOUNT_ALPN, OP_MANIFEST, OP_READ, OP_WATCH, REQUEST_HEADER_LEN, SECRET_LEN,
    wait_online,
};
use super::{WEBRTC_SIGNAL_ALPN, serve_signal};
use webrtc_transport::{IceConfig, WebRtcHandle, WebRtcTransport};

/// Producer: share `dir` read-only. Scans at startup, then rescans whenever
/// the tree changes and publishes the difference to anyone watching, so a
/// consumer sees edits without remounting. Prints the consumer's
/// `agent-share` command on stdout and serves until interrupted.
///
/// READ indices stay stable across those rescans by construction — see
/// [`super::live`], where the reason that matters is spelled out.
///
/// # Errors
/// `dir` is not a readable directory, discovery-config resolution fails, or
/// the endpoint fails to bind.
pub(crate) async fn serve(
    swarm: Option<&str>,
    flags: LookupSet,
    dir: &Path,
    json: bool,
) -> Result<()> {
    let root = dir
        .canonicalize()
        .with_context(|| format!("resolving {}", dir.display()))?;
    if !root.is_dir() {
        bail!("mount serves a directory, not a single file");
    }
    let (manifest, paths) = super::scan::scan(&root)?;
    let file_count = manifest.files.len();
    let total_bytes: u64 = manifest.files.iter().map(|file| file.size).sum();
    let encoded_len = manifest.encode().len();
    // Enforce the consumer-side cap here too: past it, every redeem would
    // abort with "manifest too large" — fail at serve time with a reason
    // instead of minting a ticket nobody can use.
    if encoded_len > usize::try_from(super::MAX_MANIFEST_BYTES).expect("u32 fits usize") {
        bail!(
            "tree too large to serve: the manifest is {} for {file_count} files (cap {})",
            human_bytes(u64::try_from(encoded_len).expect("usize fits u64")),
            human_bytes(u64::from(super::MAX_MANIFEST_BYTES))
        );
    }
    let tree = Arc::new(LiveTree::new(root.clone(), manifest, paths));
    // A watcher that cannot start is not fatal: the share still serves, it
    // just serves the startup snapshot. Losing the whole share over it would
    // be a worse trade than losing liveness.
    if let Err(error) = super::live::spawn_watcher(Arc::clone(&tree)) {
        tracing::warn!(%error, "watching the tree failed; serving a fixed snapshot");
    }

    let lookups = resolve_transfer_lookups(swarm, flags)?;
    let (endpoint, ticket, secret, webrtc) = bind(lookups).await?;
    // Shell-quoted: the hint is printed for copy-paste (and captured verbatim
    // by scripts in json mode), so a dir name with a space must stay one word.
    let mount_hint = super::shell_word(
        &root
            .file_name()
            .and_then(|name| name.to_str())
            .map_or_else(|| "./mnt".to_owned(), |name| format!("./{name}")),
    );
    super::announce(
        json,
        &format!(
            "{} ({file_count} files, {}, read-only)",
            root.display(),
            human_bytes(total_bytes)
        ),
        &format!("agent-share {} {mount_hint}", ticket.encode()),
    );

    let local_id = endpoint.id();
    let ice = IceConfig::default();
    while let Some(incoming) = endpoint.accept().await {
        let tree = Arc::clone(&tree);
        let webrtc = webrtc.clone();
        let ice = ice.clone();
        tokio::spawn(async move {
            if let Err(error) = accept_one(incoming, secret, tree, local_id, &webrtc, &ice).await {
                tracing::debug!(%error, "mount connection ended");
            }
        });
    }
    // The accept loop ended (endpoint closed) — shut down gracefully.
    endpoint.close().await;
    Ok(())
}

/// Bind the producer endpoint and mint its ticket + secret — no I/O, no print.
///
/// The endpoint answers on two ALPNs: the mount protocol, and the `WebRTC`
/// signal exchange that lets a peer with no IP path to us negotiate a data
/// channel first. The `WebRtcHandle` comes back so the accept loop can attach
/// negotiated sessions to it.
pub(super) async fn bind(
    lookups: LookupOpts,
) -> Result<(Endpoint, MountTicket, [u8; SECRET_LEN], WebRtcHandle)> {
    // Chicken and egg: the transport advertises `custom_addr(local_id)` as the
    // address peers dial it on, so it has to know the endpoint's identity —
    // but the endpoint is built *with* the transport. Mint the key first and
    // pin it, so both agree. Getting this wrong is silent: the endpoint comes
    // up fine and every WebRTC dial goes to an address nobody listens on.
    let mut key_bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut key_bytes);
    let key = SecretKey::from_bytes(&key_bytes);
    let webrtc = WebRtcHandle::new(WebRtcTransport::new(key.public()));
    let endpoint = build_endpoint(
        &lookups,
        Some(key),
        None,
        vec![MOUNT_ALPN.to_vec(), WEBRTC_SIGNAL_ALPN.to_vec()],
        Some(webrtc.clone()),
    )
    .await?;
    debug_assert_eq!(
        endpoint.id(),
        webrtc.transport().local_id(),
        "the WebRTC transport must advertise this endpoint's identity"
    );
    // Loopback needs no online wait (the bound addr is immediately usable).
    if !lookups.is_loopback() {
        wait_online(&endpoint).await;
    }
    let mut secret = [0u8; SECRET_LEN];
    rand::rng().fill_bytes(&mut secret);
    let ticket = MountTicket {
        addr: endpoint.addr(),
        secret,
        lookups,
    };
    Ok((endpoint, ticket, secret, webrtc))
}

/// Route one inbound connection by the ALPN it negotiated.
///
/// The two protocols have opposite shapes — signalling is one short exchange
/// then done, mounting is long-lived and stream-per-request — so they are
/// separated here rather than multiplexed inside one handler.
async fn accept_one(
    incoming: Incoming,
    secret: [u8; SECRET_LEN],
    tree: Arc<LiveTree>,
    local_id: EndpointId,
    webrtc: &WebRtcHandle,
    ice: &IceConfig,
) -> Result<()> {
    let conn = incoming.await.context("incoming connection failed")?;
    if conn.alpn() == WEBRTC_SIGNAL_ALPN {
        return serve_signal(&conn, local_id, webrtc, ice).await;
    }
    serve_established(conn, secret, tree).await
}

/// Serve every bi-stream on an established mount connection as an
/// independent request. Long-lived: a mounted filesystem issues reads for as
/// long as it stays mounted.
///
/// # Errors
/// The connection drops, or a stream write fails.
pub async fn serve_established(
    conn: Connection,
    secret: [u8; SECRET_LEN],
    tree: Arc<LiveTree>,
) -> Result<()> {
    // `accept_bi` errors once the connection is gone (peer closed, or a bad
    // secret closed it from within a stream task) — that ends the loop.
    while let Ok((send, recv)) = conn.accept_bi().await {
        let conn = conn.clone();
        let tree = Arc::clone(&tree);
        tokio::spawn(async move {
            if let Err(error) = serve_stream(&conn, send, recv, &secret, &tree).await {
                tracing::debug!(%error, "mount stream ended");
            }
        });
    }
    Ok(())
}

/// Authenticate one bi-stream by its 33-byte header and answer the request.
/// A bad secret closes the whole connection (the bearer is poisoned); an
/// unknown op or a malformed request drops only this stream.
async fn serve_stream(
    conn: &Connection,
    mut send: SendStream,
    mut recv: RecvStream,
    secret: &[u8; SECRET_LEN],
    tree: &LiveTree,
) -> Result<()> {
    let mut header = [0u8; REQUEST_HEADER_LEN];
    if recv.read_exact(&mut header).await.is_err() {
        // The stream died before delivering a full header — nothing to serve.
        return Ok(());
    }
    if &header[..SECRET_LEN] != secret {
        conn.close(1u32.into(), b"bad secret");
        return Ok(());
    }
    match header[SECRET_LEN] {
        OP_MANIFEST => {
            let manifest_bytes = tree.manifest_bytes();
            send.write_all(&[ReadStatus::Ok.to_byte()]).await?;
            let len = u32::try_from(manifest_bytes.len()).context("manifest too large")?;
            send.write_all(&len.to_le_bytes()).await?;
            send.write_all(&manifest_bytes).await?;
        }
        OP_WATCH => {
            // Long-lived, unlike every other op: it returns when the consumer
            // goes away, so it must not fall through to the `finish` below.
            return serve_watch(send, tree).await;
        }
        OP_READ => {
            let mut request = [0u8; 16];
            if recv.read_exact(&mut request).await.is_err() {
                return Ok(());
            }
            let index = u32::from_le_bytes(request[..4].try_into().expect("4 bytes"));
            let offset = u64::from_le_bytes(request[4..12].try_into().expect("8 bytes"));
            let len = u32::from_le_bytes(request[12..].try_into().expect("4 bytes"));
            let (status, data) = answer_read(tree, index, offset, len).await;
            send.write_all(&[status.to_byte()]).await?;
            let data_len = u32::try_from(data.len()).expect("bounded by MAX_READ_LEN");
            send.write_all(&data_len.to_le_bytes()).await?;
            send.write_all(&data).await?;
        }
        other => {
            // Unknown op: drop just this stream, keep the connection.
            tracing::debug!(op = other, "rejecting unknown mount op");
            return Ok(());
        }
    }
    // `finish` only marks the stream done; wait (briefly) for the consumer's
    // ACK so a fast/loopback connection doesn't race the stream teardown ahead
    // of the last bytes.
    let _ = send.finish();
    let _ = tokio::time::timeout(Duration::from_secs(2), send.stopped()).await;
    Ok(())
}

/// Stream tree changes until the consumer hangs up.
///
/// The opening frame is the whole manifest, so a consumer needs no separate
/// [`OP_MANIFEST`] round-trip and cannot race a change into the gap between
/// the two. Everything after it is a delta, applied in order — which is why
/// this rides one QUIC stream and why falling behind is answered with a fresh
/// manifest rather than by skipping ahead.
async fn serve_watch(mut send: SendStream, tree: &LiveTree) -> Result<()> {
    // Subscribe *before* snapshotting the manifest: the other order would drop
    // any change landing in between, and the consumer would never hear of it.
    let mut updates = tree.subscribe();
    let mut frame = tree.opening_frame();
    loop {
        let len = u32::try_from(frame.len()).context("watch frame too large")?;
        send.write_all(&[ReadStatus::Ok.to_byte()]).await?;
        send.write_all(&len.to_le_bytes()).await?;
        send.write_all(&frame).await?;
        frame = match updates.recv().await {
            Ok(next) => next.as_ref().clone(),
            Err(broadcast::error::RecvError::Lagged(missed)) => {
                // Deltas only mean anything applied in order and in full, so a
                // consumer that missed some cannot be caught up with the next
                // one. Resend the whole tree instead.
                tracing::debug!(missed, "watcher fell behind; resending the manifest");
                tree.opening_frame()
            }
            Err(broadcast::error::RecvError::Closed) => return Ok(()),
        };
    }
}

/// Serve one ranged read. Opens the file per request — simple, correct, and
/// no fd table held hostage by however many files a consumer touches; the OS
/// dentry/page cache makes the reopen cheap.
async fn answer_read(tree: &LiveTree, index: u32, offset: u64, len: u32) -> (ReadStatus, Vec<u8>) {
    if len > MAX_READ_LEN {
        return (ReadStatus::LenOverCap, Vec::new());
    }
    // Resolved through the live tree, so a tombstoned slot answers `BadIndex`
    // rather than serving whatever used to live there.
    let Some(abs) = tree.path_of(index) else {
        return (ReadStatus::BadIndex, Vec::new());
    };
    // Deliberately not clamped to the manifest's size. That size is a scan's,
    // and between rescans a file being appended to is larger than it says —
    // the clamp used to truncate every read of a growing file to its length at
    // scan time. `read_range` bounds itself against the open fd instead, so
    // the answer comes from the file as it is now.
    let want = usize::try_from(len).expect("u32 fits usize");
    match read_range(&abs, offset, want).await {
        Ok(data) => (ReadStatus::Ok, data),
        Err(error) => {
            tracing::warn!(%error, path = %abs.display(), "read failed");
            (ReadStatus::Io, Vec::new())
        }
    }
}

/// Read up to `want` bytes at `offset`, bounded by the file's live length.
///
/// The bound comes from an `fstat` on the already-open fd rather than from the
/// manifest, which serves two ends at once: a file appended to since the scan
/// reads past its recorded size, and a consumer asking for the full
/// [`MAX_READ_LEN`] of a ten-byte file still only allocates ten bytes. Reading
/// the length off the same fd we then read from also keeps the two consistent
/// under a concurrent truncate.
async fn read_range(path: &Path, offset: u64, want: usize) -> Result<Vec<u8>> {
    let mut file = tokio::fs::File::open(path).await?;
    let live = file.metadata().await?.len();
    let want = want.min(usize::try_from(live.saturating_sub(offset)).unwrap_or(want));
    if want == 0 {
        // Past the end is a valid empty read, not an error — a consumer
        // treats it as EOF.
        return Ok(Vec::new());
    }
    file.seek(std::io::SeekFrom::Start(offset)).await?;
    let mut data = vec![0u8; want];
    let mut filled = 0;
    // A plain read loop instead of `read_exact`: a file truncated between the
    // stat above and the read below yields a short (not failed) read.
    while filled < want {
        let read = file.read(&mut data[filled..]).await?;
        if read == 0 {
            break;
        }
        filled += read;
    }
    data.truncate(filled);
    Ok(data)
}
