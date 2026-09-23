use std::path::Path;
use std::sync::Arc;

use agent_share_proto::auth::ShareAuth;
use anyhow::{Context, Result, bail};
use fofoca::iroh::endpoint::Connection;
use fofoca::iroh::{Endpoint, SecretKey};
use fofoca_iroh_webrtc_transport::{IceConfig, WebRtcHandle, WebRtcTransport};
use rand::RngCore;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use super::MountTicket;
use super::ReadStatus;
use super::WEBRTC_SIGNAL_ALPN;
use super::live::LiveTree;
use super::{MAX_READ_LEN, MOUNT_ALPN, SECRET_LEN, wait_online};
use crate::file::human_bytes;
use crate::lookup::build_endpoint;
use crate::protocol::swarm::{LookupOpts, LookupSet, resolve_transfer_lookups};

/// The webapp's files view, which takes the ticket as its last path segment.
/// Must match `shareUrl()` in `packages/agent-share-web/src/lib/ticket`.
const WEB_APP_FILES_URL: &str = "https://agent-share.dev/app/files/";

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
    password: Option<&str>,
    json: bool,
) -> Result<()> {
    // Listening from the first line, so a Ctrl-C during startup also gets the
    // clean shutdown rather than the default action, which kills the process
    // without a goodbye to the mesh.
    let mut ctrl_c = tokio::spawn(tokio::signal::ctrl_c());
    let root = dir
        .canonicalize()
        .with_context(|| format!("resolving {}", dir.display()))?;
    if !root.is_dir() {
        bail!("mount serves a directory, not a single file");
    }
    let Inherited {
        secret: inherited_secret,
        auth: inherited_auth,
        mesh_id: inherited_mesh_id,
    } = inherited_from_copy(&root, password)?;
    let (authorship, named_author) = authorship_for(&root);
    let (tree, description) = open_tree(&root, authorship)?;

    let lookups = resolve_transfer_lookups(swarm, flags)?;
    // Raced too: off loopback, `bind` waits for the relay, which can take
    // seconds, and a Ctrl-C there has to be answered at once.
    let mut stopping = false;
    let bound = finish_despite_ctrl_c(
        &mut ctrl_c,
        &mut stopping,
        json,
        bind(lookups, inherited_secret),
    );
    let (endpoint, mut ticket, secret, webrtc) = bound.await?;
    if stopping {
        endpoint.close().await;
        return Ok(());
    }
    ticket.author = named_author;
    if ticket.author.is_some() {
        ticket.flags |= agent_share_proto::ticket::TICKET_FLAG_SIGNED;
    }

    // The share's real credential. On an unprotected share this is the ticket
    // secret verbatim, so everything below is byte-for-byte what it was; on a
    // protected one it is the Argon2id stretch, and the secret on its own opens
    // nothing — not the mount, and not the mesh either, since `share_mesh_key`
    // takes the token too.
    let auth = match inherited_auth {
        Some(auth) => auth,
        None => ShareAuth::new(&secret, password),
    };
    if auth.password_protected() {
        ticket.flags |= agent_share_proto::ticket::TICKET_FLAG_PASSWORD;
    }

    let mesh_target = mint_share_mesh(&auth, &secret, &ticket.lookups, password)?;
    // A re-served copy must hand out the *origin's* mesh id, not a fresh one, or
    // it splits the swarm in two.
    ticket.mesh_id = inherited_mesh_id.or_else(|| {
        mesh_target
            .as_ref()
            .filter(|_| auth.password_protected())
            .map(|target| target.mesh_id().to_owned())
    });

    let hashes = Some(open_hash_cache(auth.token()));

    // No target: the consumer mounts under the current folder by default.
    let encoded = ticket.encode();
    super::announce(json, &description, &format!("agent-share {encoded}"));
    // Human output only, for the same reason as the password note below.
    if !json {
        crate::util::output::status_out("Open", &format!("{WEB_APP_FILES_URL}{encoded}"));
    }
    // The password is deliberately *not* in that command. It travels out of
    // band — putting it in the line people paste into chat alongside the ticket
    // would defeat the whole point — so say so rather than let the recipient
    // discover it from a refused connection. Human output only: json mode is
    // read by scripts that want one runnable command and nothing else.
    if auth.password_protected() && !json {
        crate::util::output::status_out("Password", "required — send it separately");
    }

    // One endpoint, shared with the mesh — see `mount::handlers` for why the
    // accept loop had to go. The share's two ALPNs are registered on a Router
    // instead, normally the mesh's.
    let ice = IceConfig::default();
    let protocols = || share_protocols(auth, &tree, hashes.clone(), &endpoint, &webrtc, &ice);

    // The mesh normally owns the accept loop, but it must never be the reason a
    // share fails to serve. Sharing an endpoint made the mesh load-bearing for
    // *serving* — no mesh, no Router, nothing answering `MOUNT_ALPN` — which
    // quietly turned a warn-and-continue into "the share does not exist". So on
    // failure we stand up a plain Router with just the share's protocols and
    // carry on without peer counts, which is exactly the old behaviour.
    let mut fallback_router = None;
    let joined = match mesh_target {
        None => None,
        Some(target) => Some(
            finish_despite_ctrl_c(
                &mut ctrl_c,
                &mut stopping,
                json,
                super::mesh::join(super::mesh::JoinOpts {
                    // Resolved above, before the endpoint was bound: on a protected share
                    // that resolution is also where a wrong password would have been
                    // caught, and it must not wait on a background join.
                    target,
                    shared: fofoca::runtime::InjectedEndpoint {
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
                    // What this peer can actually hand over. An origin holds everything; a
                    // seed serving a partial copy holds a subset, and says so rather than
                    // letting readers discover the gaps by asking.
                    serving: tree.serving(),
                    // The producer never clears IP: it is the peer everyone else dials.
                    transports: fofoca::net::TransportOpts::default(),
                }),
            )
            .await,
        ),
    };
    let share_mesh = match joined.unwrap_or_else(|| Err(anyhow::anyhow!("no mesh for this share")))
    {
        Ok(mesh) => {
            tracing::info!(mesh = mesh.mesh_id(), "joined the share mesh");
            mesh.spawn_report(json);
            Some(mesh)
        }
        Err(error) => {
            tracing::warn!(%error, "share mesh unavailable; serving without peer discovery");
            let mut builder = fofoca::iroh::protocol::Router::builder(endpoint.clone());
            for (alpn, handler) in protocols() {
                builder = builder.accept(alpn, handler);
            }
            fallback_router = Some(builder.spawn());
            None
        }
    };

    if !stopping {
        serve_until_ctrl_c(share_mesh.as_ref(), &tree, &mut ctrl_c).await;
        super::announce_stopping(json);
    }
    if let Some(mesh) = share_mesh {
        mesh.leave().await;
    }
    drop(fallback_router);
    endpoint.close().await;
    Ok(())
}

/// Run `work` to the end even when Ctrl-C lands first, and say `Stopping` at
/// once if it does.
///
/// The work is a mesh join: dropping it half-done would drop the node's
/// endpoints without closing them, which is the ungraceful abort a clean
/// shutdown exists to avoid. `stopping` records that the Ctrl-C was used up,
/// because a finished `JoinHandle` must not be polled again.
async fn finish_despite_ctrl_c<T>(
    ctrl_c: &mut super::CtrlC,
    stopping: &mut bool,
    json: bool,
    work: impl Future<Output = T>,
) -> T {
    // Boxed: the join future is large, and pinned inline it bloats every
    // caller future up to `main`.
    let mut work = Box::pin(work);
    tokio::select! {
        done = &mut work => done,
        _ = &mut *ctrl_c => {
            *stopping = true;
            super::announce_stopping(json);
            work.await
        }
    }
}

/// Nothing to accept here any more; wait for ctrl-c so the mesh can announce
/// a graceful `Left` instead of peers waiting out a silence timeout — and,
/// while waiting, keep the card's `tree` honest.
///
/// A producer whose tree changes under the watcher would otherwise keep
/// advertising the fingerprint it started with, which is worse than
/// advertising none: a consumer would read agreement where there is none and
/// treat a diverged peer as a valid source. `set_tree` dedupes by value, so
/// the rescan timer firing with nothing changed costs nothing.
async fn serve_until_ctrl_c(
    share_mesh: Option<&super::mesh::ShareMesh>,
    tree: &LiveTree,
    ctrl_c: &mut super::CtrlC,
) {
    match share_mesh {
        Some(mesh) => {
            use tokio::sync::broadcast::error::RecvError;
            let mut updates = tree.subscribe();
            loop {
                tokio::select! {
                    _ = &mut *ctrl_c => break,
                    update = updates.recv() => match update {
                        // A lagged watcher has missed frames but the tree is
                        // still readable, so recompute rather than give up.
                        Ok(_) | Err(RecvError::Lagged(_)) => {
                            mesh.set_tree(agent_share_proto::manifest::manifest_fingerprint(
                                &tree.manifest_bytes(),
                            ))
                            .await;
                            // Availability moves for the same reasons the tree
                            // does — a file appearing or vanishing under the
                            // watcher, and a partial seed filling in. A stale
                            // grid is the same failure as a stale fingerprint:
                            // it sends readers to a peer that cannot answer.
                            // Both dedupe by value, so a rescan that changed
                            // nothing costs no gossip.
                            // A producer serves from the tree itself, so anything it can
                            // list it holds whole; the two answers coincide.
                            let serving = tree.serving();
                            let holding = serving.is_some();
                            mesh.set_serving(serving, holding).await;
                        }
                        Err(RecvError::Closed) => {
                            // The watcher is gone; the share still serves.
                            let _ = (&mut *ctrl_c).await;
                            break;
                        }
                    },
                }
            }
        }
        None => {
            let _ = ctrl_c.await;
        }
    }
}

/// What a copy carries about the share it came from, all of it optional and
/// all of it absent for an ordinary directory.
struct Inherited {
    secret: Option<[u8; SECRET_LEN]>,
    auth: Option<ShareAuth>,
    mesh_id: Option<String>,
}

/// Read the sidecar a seed left beside `root`, if this directory is a copy.
///
/// Every field exists so the copy rejoins the share it came from instead of
/// starting a rival one: the same secret so the original link still works, the
/// same token so a protected share re-seeds without being handed the password
/// again, the same mesh id so the swarm does not split in two.
///
/// # Errors
/// A copy of a protected share was given `--password`. The password was spent
/// once, at copy time; what survives is the credential it produced, so offering
/// another almost always means this is the wrong directory.
fn inherited_from_copy(root: &Path, password: Option<&str>) -> Result<Inherited> {
    let auth = super::seed::origin_auth_for(root);
    if auth.is_some() && password.is_some() {
        bail!(
            "this directory re-serves an existing share, whose password is already \
             baked into its ticket — drop --password"
        );
    }
    Ok(Inherited {
        secret: super::seed::origin_secret_for(root),
        auth,
        mesh_id: super::seed::origin_mesh_id_for(root),
    })
}

/// The key this producer signs manifests with, and the creator its ticket
/// names.
///
/// **A copy gets a key of `None` and still names an author.** That pairing is
/// the requirement in one line: a seed re-serves the signature it was handed
/// and holds nothing that could make another, so it can serve every byte of the
/// share and never publish a version of it. An original is the other way round
/// — it mints a key and names itself.
///
/// The key is per-run, like the endpoint key beside it. A restarted origin is a
/// new creator and hands out a new ticket, which is what `serve` already did
/// before any of this; persisting it would make a share's identity outlive the
/// process, and that is a separate feature with its own storage question.
fn authorship_for(root: &Path) -> (Option<SecretKey>, Option<[u8; 32]>) {
    if super::seed::is_copy(root) {
        return (None, super::seed::origin_author_for(root));
    }
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    let key = SecretKey::from_bytes(&bytes);
    let public = *key.public().as_bytes();
    (Some(key), Some(public))
}

/// The tree this directory serves, and the line describing it.
///
/// Two shapes, and which one applies is read off the directory rather than
/// asked for: a copy a seed produced carries the origin's manifest beside it,
/// and re-serving those bytes rather than scanning is what keeps every index
/// meaning what the origin says it means — and what lets a *partial* seed
/// serve at all, since a scan of a half-copy would renumber every slot after the
/// first missing file.
///
/// # Errors
/// The origin manifest is unreadable, the directory cannot be scanned, or the
/// resulting manifest is past [`super::MAX_MANIFEST_BYTES`].
fn open_tree(root: &Path, author: Option<SecretKey>) -> Result<(Arc<LiveTree>, String)> {
    if let Some(origin_bytes) = super::seed::origin_manifest_for(root) {
        let tree = Arc::new(LiveTree::seeded(root.to_path_buf(), origin_bytes)?);
        let (held, total) = tree.coverage();
        let description = format!(
            "{} (re-seeding another share: {held} of {total} files held, read-only)",
            root.display()
        );
        return Ok((tree, description));
    }
    let (manifest, paths) = super::scan::scan(root)?;
    let file_count = manifest.files.len();
    let total_bytes: u64 = manifest.files.iter().map(|file| file.size).sum();
    let encoded_len = manifest.encode().len();
    // Enforce the consumer-side cap here too: past it, every redeem would abort
    // with "manifest too large" — fail at serve time with a reason instead of
    // minting a ticket nobody can use.
    if encoded_len > usize::try_from(super::MAX_MANIFEST_BYTES).expect("u32 fits usize") {
        bail!(
            "tree too large to serve: the manifest is {} for {file_count} files (cap {})",
            human_bytes(u64::try_from(encoded_len).expect("usize fits u64")),
            human_bytes(u64::from(super::MAX_MANIFEST_BYTES))
        );
    }
    let tree = Arc::new(LiveTree::authored(
        root.to_path_buf(),
        manifest,
        paths,
        author,
    ));
    // A watcher that cannot start is not fatal: the share still serves, it just
    // serves the startup snapshot. Losing the whole share over it would be a
    // worse trade than losing liveness.
    if let Err(error) = super::live::spawn_watcher(Arc::clone(&tree)) {
        tracing::warn!(%error, "watching the tree failed; serving a fixed snapshot");
    }
    let description = format!(
        "{} ({file_count} files, {}, read-only)",
        root.display(),
        human_bytes(total_bytes)
    );
    Ok((tree, description))
}

/// The mesh this producer joins, or `None` when it cannot join one.
///
/// `None` has exactly one cause: a seed re-serving a *protected* share it was
/// given no password for. fofoca gates every mesh derivation behind the
/// stretched password key, so such a peer genuinely cannot join — the token in
/// its sidecar opens the mount protocol but says nothing about the mesh. It
/// still serves every byte it holds to whoever dials it; it just does not appear
/// on the roster. Serving nothing would be the worse trade.
///
/// # Errors
/// The mesh id cannot be derived.
fn mint_share_mesh(
    auth: &ShareAuth,
    secret: &[u8; SECRET_LEN],
    lookups: &LookupOpts,
    password: Option<&str>,
) -> Result<Option<super::mesh::ShareMeshTarget>> {
    if auth.password_protected() && password.is_none() {
        tracing::warn!(
            "re-serving a password-protected share without its password: \
             serving reads, but not joining the share's mesh"
        );
        return Ok(None);
    }
    Ok(Some(
        super::mesh::mint(secret, lookups, password).context("minting the share's mesh")?,
    ))
}

/// The share's two ALPNs, ready for the mesh's Router.
fn share_protocols(
    auth: ShareAuth,
    tree: &Arc<LiveTree>,
    hashes: Option<Arc<super::hash::ChunkCache>>,
    endpoint: &Endpoint,
    webrtc: &WebRtcHandle,
    ice: &IceConfig,
) -> Vec<(Vec<u8>, Box<dyn fofoca::iroh::protocol::DynProtocolHandler>)> {
    vec![
        (
            MOUNT_ALPN.to_vec(),
            Box::new(super::handlers::MountHandler::new(
                auth,
                super::source::NativeSource::Producer(super::source::ProducerSource::new(
                    Arc::clone(tree),
                    hashes,
                )),
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
}

/// Open this share's hash cache, or `None` if it cannot be opened.
///
/// Keyed by the share's mesh id, which is already a one-way hash of the token —
/// see `mesh_key` for why the credential itself must never reach a path.
///
/// Never fatal. Without a cache a consumer cannot verify bytes from a third
/// party and falls back to reading from this origin, which is exactly today's
/// behaviour.
fn open_hash_cache(_token: &[u8; SECRET_LEN]) -> Arc<super::hash::ChunkCache> {
    // Always available now, and holding nothing until somebody asks. The table
    // is in memory rather than a sidecar directory, so there is no longer an
    // open that can fail — and nothing on disk that could describe content
    // which has since moved.
    //
    // The callers still take an `Option`, because "a producer with no chunk
    // table" is a state the tests exercise and the protocol has an answer for.
    Arc::new(super::hash::ChunkCache::new())
}

/// Bind the producer endpoint and mint its ticket + secret — no I/O, no print.
///
/// The endpoint answers on two ALPNs: the mount protocol, and the `WebRTC`
/// signal exchange that lets a peer with no IP path to us negotiate a data
/// channel first. The `WebRtcHandle` comes back so the accept loop can attach
/// negotiated sessions to it.
/// `inherited` is `Some` when serving a directory a seed produced. Minting a
/// fresh secret there would build a *second* share: its own mesh id, its own
/// ticket, and no way for anyone holding the original link to discover it.
/// Adopting the origin's secret is what makes a copy an extra source for the
/// share it came from — the peers already looking for it find it, and the link
/// they were given keeps working after the origin is gone.
///
/// The endpoint key is still fresh. The secret is the *share* capability, not
/// this peer's identity, and two peers must never share the latter.
pub(super) async fn bind(
    lookups: LookupOpts,
    inherited: Option<[u8; SECRET_LEN]>,
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
    let secret = inherited.unwrap_or_else(|| {
        let mut minted = [0u8; SECRET_LEN];
        rand::rng().fill_bytes(&mut minted);
        minted
    });
    let ticket = MountTicket {
        addr: endpoint.addr(),
        secret,
        lookups,
        kind: agent_share_proto::ticket::TICKET_KIND_SHARE,
        // The caller sets `TICKET_FLAG_PASSWORD` if it protected the share;
        // binding an endpoint knows nothing about that.
        flags: 0,
        mesh_id: None,
        author: None,
    };
    Ok((endpoint, ticket, secret, webrtc))
}

/// Serve every bi-stream on an established mount connection as an
/// independent request. Long-lived: a mounted filesystem issues reads for as
/// long as it stays mounted.
///
/// # Errors
/// The connection drops, or a stream write fails.
pub(super) async fn serve_established(
    conn: Connection,
    auth: ShareAuth,
    source: super::source::NativeSource,
) -> Result<()> {
    // `accept_bi` errors once the connection is gone (peer closed, or a bad
    // token closed it from within a stream task) — that ends the loop.
    while let Ok((send, recv)) = conn.accept_bi().await {
        let conn = conn.clone();
        // The dispatch itself is `agent_share_mount`'s, shared with the
        // browser. All that is left here is which source answers it.
        let source = source.clone();
        tokio::spawn(async move {
            if let Err(error) =
                agent_share_mount::serve_stream(&conn, send, recv, &auth, source).await
            {
                tracing::debug!(%error, "mount stream ended");
            }
        });
    }
    Ok(())
}

/// Serve one ranged read. Opens the file per request — simple, correct, and
/// no fd table held hostage by however many files a consumer touches; the OS
/// dentry/page cache makes the reopen cheap.
pub(super) async fn answer_read(
    tree: &LiveTree,
    index: u32,
    offset: u64,
    len: u32,
) -> (ReadStatus, Vec<u8>) {
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
