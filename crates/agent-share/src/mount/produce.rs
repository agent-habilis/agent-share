use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use iroh::endpoint::{Connection, RecvStream, SendStream};
use iroh::{Endpoint, SecretKey};
use rand::RngCore;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::broadcast;

use crate::file::human_bytes;
use crate::lookup::build_endpoint;
use crate::protocol::swarm::{LookupOpts, LookupSet, resolve_transfer_lookups};

use super::MountTicket;
use super::ReadStatus;
use super::WEBRTC_SIGNAL_ALPN;
use super::live::LiveTree;
use super::{
    MAX_READ_LEN, MOUNT_ALPN, OP_MANIFEST, OP_READ, OP_WATCH, REQUEST_HEADER_LEN, SECRET_LEN,
    wait_online,
};
use fofoca_iroh_webrtc_transport::{IceConfig, WebRtcHandle, WebRtcTransport};

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
    // Target parent for the consumer — it creates `agent-share-…/` under this.
    let mount_hint = super::shell_word(".");
    super::announce(
        json,
        &format!(
            "{} ({file_count} files, {}, read-only)",
            root.display(),
            human_bytes(total_bytes)
        ),
        &format!("agent-share {} {mount_hint}", ticket.encode()),
    );

    // One endpoint, shared with the mesh — see `mount::handlers` for why the
    // accept loop had to go. The share's two ALPNs are registered on a Router
    // instead, normally the mesh's.
    let ice = IceConfig::default();
    let protocols = || -> Vec<(Vec<u8>, Box<dyn iroh::protocol::DynProtocolHandler>)> {
        vec![
            (
                MOUNT_ALPN.to_vec(),
                Box::new(super::handlers::MountHandler::new(
                    secret,
                    Arc::clone(&tree),
                )),
            ),
            (
                WEBRTC_SIGNAL_ALPN.to_vec(),
                Box::new(super::handlers::SignalHandler::new(
                    endpoint.id(),
                    webrtc.clone(),
                    ice.clone(),
                )),
            ),
        ]
    };

    // The mesh normally owns the accept loop, but it must never be the reason a
    // share fails to serve. Sharing an endpoint made the mesh load-bearing for
    // *serving* — no mesh, no Router, nothing answering `MOUNT_ALPN` — which
    // quietly turned a warn-and-continue into "the share does not exist". So on
    // failure we stand up a plain Router with just the share's protocols and
    // carry on without peer counts, which is exactly the old behaviour.
    let mut fallback_router = None;
    let share_mesh = match super::mesh::join(super::mesh::JoinOpts {
        secret: &secret,
        // Read off the ticket, not off the local `lookups` binding that `bind`
        // consumed. Same value, but this way the invariant — every peer of this
        // share derives the mesh from what the ticket says — is literal.
        lookups: &ticket.lookups,
        shared: agent_habilis_mesh::runtime::InjectedEndpoint {
            endpoint: endpoint.clone(),
            webrtc: webrtc.clone(),
        },
        protocols: protocols(),
        role: super::mesh::Role::Producer,
        // Over the bytes `OP_MANIFEST` actually serves, not a re-encode of the
        // struct. The producer holds them, so it can fingerprint the exact
        // thing a consumer will hash on the other side.
        tree: Some(agent_share_proto::manifest::manifest_fingerprint(
            &tree.manifest_bytes(),
        )),
        // The producer never clears IP: it is the peer everyone else dials.
        transports: agent_habilis_mesh::net::TransportOpts::default(),
    })
    .await
    {
        Ok(mesh) => {
            tracing::info!(mesh = mesh.mesh_id(), "joined the share mesh");
            mesh.spawn_report(json);
            Some(mesh)
        }
        Err(error) => {
            tracing::warn!(%error, "share mesh unavailable; serving without peer discovery");
            let mut builder = iroh::protocol::Router::builder(endpoint.clone());
            for (alpn, handler) in protocols() {
                builder = builder.accept(alpn, handler);
            }
            fallback_router = Some(builder.spawn());
            None
        }
    };

    // Nothing to accept here any more; wait for ctrl-c so the mesh can announce
    // a graceful `Left` instead of peers waiting out a silence timeout — and,
    // while waiting, keep the card's `tree` honest.
    //
    // A producer whose tree changes under the watcher would otherwise keep
    // advertising the fingerprint it started with, which is worse than
    // advertising none: a consumer would read agreement where there is none and
    // treat a diverged peer as a valid source. `set_tree` dedupes by value, so
    // the rescan timer firing with nothing changed costs nothing.
    match &share_mesh {
        Some(mesh) => {
            use tokio::sync::broadcast::error::RecvError;
            let mut updates = tree.subscribe();
            loop {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => break,
                    update = updates.recv() => match update {
                        // A lagged watcher has missed frames but the tree is
                        // still readable, so recompute rather than give up.
                        Ok(_) | Err(RecvError::Lagged(_)) => {
                            mesh.set_tree(agent_share_proto::manifest::manifest_fingerprint(
                                &tree.manifest_bytes(),
                            ))
                            .await;
                        }
                        Err(RecvError::Closed) => {
                            // The watcher is gone; the share still serves.
                            let _ = tokio::signal::ctrl_c().await;
                            break;
                        }
                    },
                }
            }
        }
        None => {
            let _ = tokio::signal::ctrl_c().await;
        }
    }
    if let Some(mesh) = share_mesh {
        mesh.leave().await;
    }
    drop(fallback_router);
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
        false,
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
        kind: agent_share_proto::ticket::TICKET_KIND_SHARE,
    };
    Ok((endpoint, ticket, secret, webrtc))
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
