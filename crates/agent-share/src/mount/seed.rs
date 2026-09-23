//! `agent-share seed` — take a full copy of a share, then serve it.
//!
//! A separate verb from the lazy mount, deliberately. The mount is diskless: it
//! fetches bytes as a reader touches them and keeps nothing, which is what makes
//! browsing a 500 GB share cheap. A seed is the opposite trade, taken on
//! purpose — it downloads everything so that this machine can hand it to
//! somebody else. **A peer that holds no bytes cannot seed, and no protocol
//! design removes that.**
//!
//! Its output is an ordinary directory, so the serve step is the verb that
//! already exists, run on the copy once it is complete. `--copy-only` stops
//! before that step, and the copy can be served later:
//!
//! ```sh
//! agent-share seed <ticket> ./copy               # copy, then serve
//! agent-share seed <ticket> ./copy --copy-only   # copy only
//! agent-share serve ./copy                       # serve it later
//! ```
//!
//! # Verifying without a verified transport
//!
//! `OP_READ` returns raw bytes, not a bao stream, so a seed cannot verify
//! *ranges* as they arrive — that needs the range protocol stage 4 brings. What
//! it can do is better than nothing and cheap: it downloads a whole file, hashes
//! it locally, and compares the result with the root the origin reports over
//! `OP_HASH`. A mismatch means the bytes were mangled between here and there, so
//! the file is refused rather than written.
//!
//! The hash is also what makes the copy *useful*. Recording it binds this
//! machine's file to the same root the origin published, so a later reader can
//! be told "fetch it from over there, check it against this" and get the same
//! guarantee it would have got from the origin.

use std::path::{Path, PathBuf};

use agent_share_proto::auth::ShareAuth;
use agent_share_proto::authorship::SignedManifest;
use anyhow::{Context, Result, bail};
use fofoca_chunks::ChunkMap;

use super::MountTicket;
use super::consume::RemoteClient;
use crate::file::human_bytes;
use crate::lookup::add_peer_addr;

/// One file's outcome, for the summary line.
#[derive(Default)]
struct Tally {
    files: usize,
    bytes: u64,
    /// Files whose local hash matched the root the origin published.
    verified: usize,
    /// Files the origin could not vouch for. Not an error: an origin with no
    /// hash cache says this about everything, and the copy is still correct —
    /// it just cannot be *proved* correct from here.
    unverified: usize,
    /// Files a `--only` filter left behind.
    skipped: usize,
}

/// Filename holding the origin's manifest bytes inside the sidecar.
pub(super) const ORIGIN_MANIFEST: &str = "origin.manifest";

/// Filename holding the share secret this copy belongs to.
///
/// **Why a seed keeps the secret.** Serving a copy under a *fresh* secret
/// would make a second, unrelated share: a different mesh, a different ticket,
/// and nobody holding the original link would ever find it. Re-serving under the
/// origin's secret is what makes a seed an additional *source for the same
/// share* — the thing a swarm is.
///
/// The protocol already allows this and needs no new code for it: a producer
/// authenticates a read by comparing the secret and nothing else, with no
/// binding to who is serving. That is asserted by
/// `mount::tests::a_non_origin_peer_serves_the_origins_ticket_secret`.
///
/// It is the read capability at rest, so it is written with owner-only
/// permissions. Whoever ran the seed already holds it — it came in the ticket
/// they pasted — so this stores nothing they did not have. It does mean a
/// seed directory is as sensitive as the link that made it.
pub(super) const ORIGIN_SECRET: &str = "origin.secret";

/// Filename holding the *token* a protected share's password derived, written
/// only when the share was protected.
///
/// Two files rather than one because they answer different questions.
/// [`ORIGIN_SECRET`] is what goes back into the ticket this copy hands out, so
/// the link people already have keeps working. This is what a request must
/// present, and on a protected share the two are not the same bytes.
///
/// **Why the token and not the password.** The password is a human secret,
/// likely reused; the token is a per-share credential that opens this share and
/// nothing else. Storing the token lets a seed re-seed unattended — the point
/// of seeding — without ever putting the password on disk. It is the same
/// class of secret [`ORIGIN_SECRET`] already is, written the same way (0600),
/// and it makes the seed directory exactly as sensitive as the ticket *plus*
/// the password that made it.
pub(super) const ORIGIN_AUTH: &str = "origin.auth";

/// Filename holding the origin's **mesh id**, written only for a protected
/// share.
///
/// A copy has to hand out the id the origin minted, not one of its own: the id
/// carries the password verifier, and two different ids are two different
/// meshes — which would split the swarm exactly when seeders are what keeps the
/// share alive. Not a secret on its own (it opens nothing without the
/// password), but it is written beside the two that are.
pub(super) const ORIGIN_MESH: &str = "origin.mesh";

/// Filename holding the creator's **authorship public key**, so the ticket this
/// copy hands out names the same creator the original ticket did.
///
/// The public half only, and the only file in the sidecar that is not a
/// capability: it verifies manifests and signs nothing. That asymmetry is the
/// point of the key split — a seed is given everything it needs to serve the
/// share and nothing that would let it publish a new version of it. See
/// [`agent_share_proto::authorship`].
pub(super) const ORIGIN_AUTHOR: &str = "origin.author";

/// Whether `rel_path` was asked for.
///
/// An empty filter means everything, so the ordinary whole-share seed needs
/// no special case. A filter entry matches the file itself or any file beneath
/// it, so naming a directory takes the directory.
fn wanted(only: &[String], rel_path: &str) -> bool {
    if only.is_empty() {
        return true;
    }
    only.iter().any(|want| {
        let want = want.trim_matches('/');
        rel_path == want || rel_path.starts_with(&format!("{want}/"))
    })
}

/// Copy every file in the share behind `ticket` into `dest`.
///
/// # Errors
/// The ticket does not decode, the share is unreachable, `dest` cannot be
/// written, or a file fails to verify against the root the origin published.
pub(crate) async fn seed(
    ticket: &str,
    dest: &Path,
    only: &[String],
    webrtc_only: bool,
    password: Option<&str>,
    copy_only: bool,
    json: bool,
) -> Result<()> {
    let ticket = MountTicket::decode(ticket)?;
    let webrtc_only = webrtc_only || super::consume::relay_only(&ticket.addr);
    // Kept so the copy can be re-served as a source for *this* share rather
    // than as a new one. See `ORIGIN_SECRET`.
    let secret = ticket.secret;
    // Fails here, before a byte is fetched, if the password is missing or the
    // ticket does not want one.
    // Resolves the mesh too, which is what makes a wrong password fail here
    // rather than after a ninety-second discovery deadline. A seed never joins
    // that mesh — it only needs the ruling.
    let auth = super::consume::redeem_auth(&ticket, password)?.auth;
    // Kept so the copy re-serves the origin's mesh rather than minting a rival.
    let mesh_id = ticket.mesh_id.clone();
    // The creator's public key, so the copy names the same author. Public: this
    // is what a copy is *given*, unlike the key that would let it publish.
    let author = ticket.author;
    // The same endpoint the mount form builds, for the same reason: it carries
    // the WebRTC handle, without which `--transport webrtc` has no lane to be
    // forced onto.
    let (endpoint, webrtc) = super::consume::consumer_endpoint(&ticket, webrtc_only).await?;
    add_peer_addr(&endpoint, ticket.addr.clone())?;
    let client = RemoteClient::new(endpoint.clone(), ticket, auth)
        .with_webrtc(webrtc.clone())
        .webrtc_only(webrtc_only);

    // Whatever happens below, close the endpoint. Dropping it instead aborts
    // ungracefully and prints an iroh error over the top of ours, which buries
    // the reason a seed actually failed.
    let outcome = copy_all(
        &client,
        dest,
        only,
        &OriginFacts {
            secret,
            auth,
            mesh_id: mesh_id.as_deref(),
            author,
        },
        json,
    )
    .await;
    // Read before the endpoint closes: the close reason lives on the connection
    // the client is holding, and closing the endpoint takes it with it.
    let refused = client.refused_for_password().await;
    endpoint.close().await;
    // A refused credential reaches here as "connection lost / closed by peer",
    // which reads as a flaky network. Name it, the same way `attach` does.
    let tally = outcome.map_err(|error| {
        if refused {
            error.context(super::consume::WRONG_PASSWORD)
        } else {
            error
        }
    })?;
    report(&tally, dest, copy_only, json);
    Ok(())
}

/// The origin's manifest — as a difference when this destination already holds
/// one, and whole when it does not.
///
/// **A re-run into a folder seeded before is the common case**, and it used to
/// pay for the tree again to learn that almost nothing moved: on a large share
/// that is several MB fetched to discover a handful of changed files. The
/// sidecar already keeps the envelope this copy was built from, so the version
/// is right there to ask from.
///
/// Every failure falls back to the whole manifest, including a sidecar that does
/// not decode — a copy left half-written by an interrupted run must not be able
/// to stop a later one from working.
async fn fetch_manifest_for(client: &RemoteClient, dest: &Path) -> Result<SignedManifest> {
    let held = origin_manifest_for(dest).and_then(|bytes| SignedManifest::decode(&bytes).ok());
    if let Some(held) = held {
        match client.fetch_manifest_since(&held).await {
            Ok(Some(caught_up)) => {
                tracing::debug!(
                    from = held.version,
                    to = caught_up.version,
                    "caught the seed's manifest up by difference"
                );
                return Ok(caught_up);
            }
            // The producer cannot answer from that far back — or is a seeder, or
            // predates the op. Ordinary; ask for the tree.
            Ok(None) => {}
            // It answered with something that does not rebuild what the creator
            // signed. Worth a line: the fallback hides it otherwise, and a
            // producer doing this is not the same as one that cannot answer.
            Err(error) => {
                tracing::warn!(%error, "the difference did not verify; fetching the whole manifest");
            }
        }
    }
    client.fetch_signed_manifest().await
}

/// What the copy has to remember about the share it came from, so that
/// `agent-share serve` on it rejoins that share rather than starting a rival.
///
/// Grouped because they travel together and are written together: every one of
/// them lands in the sidecar before a single byte of content is fetched.
struct OriginFacts<'a> {
    secret: [u8; agent_share_proto::framing::SECRET_LEN],
    auth: ShareAuth,
    mesh_id: Option<&'a str>,
    /// The creator's public key. Not a capability — see [`ORIGIN_AUTHOR`].
    author: Option<[u8; 32]>,
}

/// The body of a seed, so its caller can close the endpoint either way.
async fn copy_all(
    client: &RemoteClient,
    dest: &Path,
    only: &[String],
    origin: &OriginFacts<'_>,
    json: bool,
) -> Result<Tally> {
    let OriginFacts {
        secret,
        auth,
        mesh_id,
        author,
    } = *origin;
    // The *bytes*, not just the decoded struct. A seed re-serves these
    // verbatim so its indices stay the origin's — see `LiveTree::seeded` — and
    // the creator's signature with them, since a copy has no way to make one.
    let signed = fetch_manifest_for(client, dest).await?;
    let envelope = signed.encode();
    let manifest = agent_share_proto::manifest::MountManifest::decode(&signed.manifest)?;
    std::fs::create_dir_all(dest).with_context(|| format!("creating {}", dest.display()))?;

    // Directories first, and *all* of them: the manifest lists every directory
    // rather than only non-empty ones, so an empty one in the original stays an
    // empty one here rather than quietly vanishing.
    for dir in &manifest.dirs {
        let path = safe_join(dest, &dir.rel_path)?;
        std::fs::create_dir_all(&path).with_context(|| format!("creating {}", path.display()))?;
    }

    // The store lives *beside* the copy, not inside it, so `agent-share serve`
    // on the destination shares the user's files and not our bookkeeping.
    let sidecar = sidecar_dir(dest);
    // The sidecar still exists — it carries the origin's manifest, secret and
    // mesh id — but it no longer holds a byte of content. A seed re-serves
    // the copy it just wrote, in place, and addresses it lazily the way the
    // origin does; keeping a second copy under here would double the disk cost
    // of seeding for nothing.
    std::fs::create_dir_all(&sidecar).with_context(|| format!("creating {}", sidecar.display()))?;
    // Kept so `serve` can re-serve the origin's manifest rather than deriving
    // one from this directory. Written before any byte is fetched, so even an
    // interrupted seed is re-servable for what it did get.
    std::fs::create_dir_all(&sidecar).with_context(|| format!("creating {}", sidecar.display()))?;
    std::fs::write(sidecar.join(ORIGIN_MANIFEST), &envelope)
        .context("recording the origin manifest")?;
    write_secret(&sidecar.join(ORIGIN_SECRET), &secret).context("recording the share secret")?;
    // Public, so no 0600 and nothing here a copy could sign with. It is written
    // so the ticket this copy hands out names the same creator the original did
    // — otherwise a reader arriving through the copy would have nothing to check
    // the re-served signature against.
    if let Some(author) = author {
        std::fs::write(sidecar.join(ORIGIN_AUTHOR), author).context("recording the author key")?;
    }
    // Only for a protected share. Its absence is what tells `serve` the secret
    // alone is the credential, so an ordinary seed is untouched by any of
    // this — no extra file, no extra read.
    if auth.password_protected() {
        write_secret(&sidecar.join(ORIGIN_AUTH), auth.token())
            .context("recording the share token")?;
        if let Some(mesh_id) = mesh_id {
            std::fs::write(sidecar.join(ORIGIN_MESH), mesh_id)
                .context("recording the share mesh id")?;
        }
    }

    let mut tally = Tally::default();
    for (index, file) in manifest.files.iter().enumerate() {
        // Tombstones hold a slot open so later indices keep meaning what they
        // meant; there is no file to copy.
        if file.is_tombstone() {
            continue;
        }
        // On-demand: fetch only what was asked for. A peer that wants one file
        // out of a thousand takes one file, and can then seed that one — the
        // manifest it re-serves still describes the whole tree, with the rest
        // answered as "not here".
        if !wanted(only, &file.rel_path) {
            tally.skipped += 1;
            continue;
        }
        let index = u32::try_from(index).context("manifest index fits u32")?;
        let path = safe_join(dest, &file.rel_path)?;
        if !json {
            crate::util::output::status("Fetching", &file.rel_path);
        }
        let bytes = fetch_whole(client, index, file.size)
            .await
            .with_context(|| {
                format!(
                    "{} is listed in the manifest but this peer would not serve it \
                 (a partial seed holds only some of a share)",
                    file.rel_path
                )
            })?;
        write_file(&path, &bytes)?;
        let verified = record(client, index, &path, &bytes).await?;

        tally.files += 1;
        tally.bytes += bytes.len() as u64;
        if verified {
            tally.verified += 1;
        } else {
            tally.unverified += 1;
        }
    }

    Ok(tally)
}

/// Where a seed keeps what it knows about the copy.
///
/// A sibling of the destination rather than a child, because the destination is
/// meant to be handed straight to `agent-share serve` and anything inside it
/// would be served as part of the share.
fn sidecar_dir(dest: &Path) -> PathBuf {
    let name = dest.file_name().map_or_else(
        || "seed".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    dest.parent()
        .unwrap_or(Path::new("."))
        .join(format!(".{name}.agent-share"))
}

/// Write a secret with owner-only permissions.
fn write_secret(path: &Path, secret: &[u8]) -> Result<()> {
    std::fs::write(path, secret).with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        // 0600. The read capability should not be world-readable just because
        // it happens to live in a cache directory.
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("restricting {}", path.display()))?;
    }
    Ok(())
}

/// The share secret a seed left beside `root`, if this directory is one.
///
/// Serving with this rather than a fresh secret is what puts the copy on the
/// *same* mesh as the origin, answering the *same* ticket — so every holder of
/// the original link gains a source without being told anything.
pub(super) fn origin_secret_for(
    root: &Path,
) -> Option<[u8; agent_share_proto::framing::SECRET_LEN]> {
    let bytes = std::fs::read(sidecar_dir(root).join(ORIGIN_SECRET)).ok()?;
    bytes.try_into().ok()
}

/// The origin's manifest bytes a seed left beside `root`, if this directory
/// is one.
///
/// Presence of this file is what distinguishes a seed from an ordinary
/// directory, and it is why `serve` does not need a flag: a copy knows what it
/// is a copy of.
pub(super) fn origin_manifest_for(root: &Path) -> Option<Vec<u8>> {
    std::fs::read(sidecar_dir(root).join(ORIGIN_MANIFEST)).ok()
}

/// Whether `root` is a copy of somebody else's share.
///
/// The same question [`origin_manifest_for`] answers, without reading a
/// manifest that can run to megabytes — the caller that only needs the ruling
/// asks this one.
pub(super) fn is_copy(root: &Path) -> bool {
    sidecar_dir(root).join(ORIGIN_MANIFEST).exists()
}

/// The credential a seed of a *protected* share left beside `root`.
///
/// `None` for an ordinary seed, whose secret is its own credential — so
/// `serve` derives one from [`origin_secret_for`] instead and behaves exactly as
/// it always has. See [`ORIGIN_AUTH`] for why the token is what is kept.
pub(super) fn origin_mesh_id_for(root: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(sidecar_dir(root).join(ORIGIN_MESH)).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_owned())
}

/// The creator's authorship public key a seed left beside `root`.
///
/// `None` for a copy of an unsigned share, and for a directory that is not a
/// copy at all — both mean "this producer has no creator to name".
pub(super) fn origin_author_for(root: &Path) -> Option<[u8; 32]> {
    let bytes = std::fs::read(sidecar_dir(root).join(ORIGIN_AUTHOR)).ok()?;
    bytes.try_into().ok()
}

/// The credential a seed of a *protected* share left beside `root`.
pub(super) fn origin_auth_for(root: &Path) -> Option<ShareAuth> {
    let bytes = std::fs::read(sidecar_dir(root).join(ORIGIN_AUTH)).ok()?;
    let token: [u8; agent_share_proto::framing::SECRET_LEN] = bytes.try_into().ok()?;
    Some(ShareAuth::from_token(token, true))
}

/// Read a whole file, by chunk address where the peer can address, else by
/// byte range.
///
/// Preferring addresses is not a micro-optimisation. A peer holding *part* of
/// a file can answer for the chunks it has and decline the rest, so a seed
/// can be assembled from several partial seeders — which is exactly the case a
/// dead origin leaves behind. `OP_READ` can only ever ask one peer for a range
/// and take what it gets.
///
/// Every chunk is verified against the row before it is used, so the bytes are
/// safe to take from anyone.
async fn fetch_whole(client: &RemoteClient, index: u32, size: u64) -> Result<Vec<u8>> {
    if let Some(row) = client.fetch_chunk_map(index).await? {
        let mut bytes = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
        for position in 0..row.len() {
            let address = row
                .leaf(position)
                .context("a chunk row is shorter than it says")?;
            // `None` means the peer addressed the file but cannot serve this
            // chunk — ordinary for a partial holder. Falling back to a byte
            // range is what a whole-file holder can always answer.
            let chunk = if let Some(chunk) = client.fetch_chunk(address).await? {
                chunk
            } else {
                let range = row.range_of(position);
                let len =
                    u32::try_from(range.end - range.start).context("a chunk is under 4 GiB")?;
                read_exact_range(client, index, range.start, len).await?
            };
            if !row.verify(position, &chunk) {
                bail!("a peer answered chunk {position} of {index} with the wrong bytes");
            }
            bytes.extend_from_slice(&chunk);
        }
        return Ok(bytes);
    }

    // No row: this peer cannot address the file at all, so read it as bytes.
    let mut bytes = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
    let mut offset = 0u64;
    loop {
        let chunk = client
            .read_range(index, offset, super::MAX_READ_LEN)
            .await?;
        if chunk.is_empty() {
            // A short read means EOF from the origin — see `answer_read`, which
            // deliberately does not clamp to the scanned size so that a growing
            // file can be read past it.
            break;
        }
        offset += chunk.len() as u64;
        bytes.extend_from_slice(&chunk);
        if offset >= size {
            break;
        }
    }
    Ok(bytes)
}

/// Read exactly `len` bytes at `offset`, looping over the protocol's cap.
async fn read_exact_range(
    client: &RemoteClient,
    index: u32,
    offset: u64,
    len: u32,
) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(usize::try_from(len).unwrap_or(0));
    loop {
        let taken = u32::try_from(out.len()).context("a chunk is under 4 GiB")?;
        if taken >= len {
            break;
        }
        let piece = client
            .read_range(index, offset + u64::from(taken), len - taken)
            .await?;
        if piece.is_empty() {
            bail!("the peer stopped short of the size its own chunk row describes");
        }
        out.extend_from_slice(&piece);
    }
    Ok(out)
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))
}

/// Address the copy and cross-check it against the origin.
///
/// Returns whether the origin was able to vouch for the content *and* agreed.
///
/// Comparing roots rather than bytes is the whole point of the row: the origin
/// never sends its content twice, and a single 32-byte answer settles whether
/// what landed here is what it meant to send.
async fn record(client: &RemoteClient, index: u32, path: &Path, bytes: &[u8]) -> Result<bool> {
    let ours = ChunkMap::build(bytes);

    // `None` is ordinary — the peer keeps no chunk table, or cannot address
    // that index. The copy stands; it simply is not provable from here.
    let Some(theirs) = client.fetch_chunk_map(index).await? else {
        return Ok(false);
    };
    if ours.root() != theirs.root() {
        bail!(
            "{} does not match the origin: the bytes were altered in transit, \
             or the origin is serving content it did not address",
            path.display()
        );
    }
    Ok(true)
}

/// Join `rel` under `dest`, refusing anything that would escape it.
///
/// A manifest arrives over the network, so `../..` in a path is an attack, not
/// a typo. `nfs.rs` rejects the same shapes on the mount side; a seed writes
/// to disk and so has to reject them here too.
fn safe_join(dest: &Path, rel: &str) -> Result<PathBuf> {
    let mut path = dest.to_path_buf();
    for part in rel.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." || part.contains('\\') || part.contains('\0') {
            bail!("refusing a manifest path that escapes the destination: {rel:?}");
        }
        path.push(part);
    }
    if !path.starts_with(dest) {
        bail!("refusing a manifest path that escapes the destination: {rel:?}");
    }
    Ok(path)
}

fn report(tally: &Tally, dest: &Path, copy_only: bool, json: bool) {
    // Without `--copy-only` the serve step prints the one line that matters
    // next, so a hint to serve would only be noise.
    if json {
        if copy_only {
            println!("agent-share serve {}", dest.display());
        }
        return;
    }
    let skipped = if tally.skipped == 0 {
        String::new()
    } else {
        format!(", {} not requested", tally.skipped)
    };
    crate::util::output::status_out(
        "Copied",
        &format!(
            "{} files, {} — {} verified against the origin, {} unverified{skipped}",
            tally.files,
            human_bytes(tally.bytes),
            tally.verified,
            tally.unverified
        ),
    );
    if !copy_only {
        return;
    }
    crate::util::output::status_out(
        "Serve",
        &format!(
            "agent-share serve {}",
            super::shell_word(&dest.to_string_lossy())
        ),
    );
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::wanted;
    use super::{ORIGIN_SECRET, origin_secret_for, safe_join, sidecar_dir, write_secret};

    /// **The property that makes a copy a source rather than a rival share.**
    ///
    /// Serving under a freshly minted secret would build a second mesh with a
    /// second ticket, and nobody holding the original link would ever find it.
    /// `produce::serve` reads this back and adopts it, which is why a seed
    /// re-seeds the share it came from.
    #[test]
    fn a_seed_hands_its_secret_back_to_serve() {
        let root = std::env::temp_dir().join(format!(
            "agent-share-secret-{}-{}",
            std::process::id(),
            line!()
        ));
        let sidecar = sidecar_dir(&root);
        std::fs::create_dir_all(&sidecar).expect("sidecar");

        assert_eq!(
            origin_secret_for(&root),
            None,
            "a directory nobody seeded has no secret to adopt"
        );

        let secret = [7u8; agent_share_proto::framing::SECRET_LEN];
        write_secret(&sidecar.join(ORIGIN_SECRET), &secret).expect("write");
        assert_eq!(origin_secret_for(&root), Some(secret));

        // It is the read capability at rest. A cache directory is no reason for
        // it to be world-readable.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(sidecar.join(ORIGIN_SECRET))
                .expect("stat")
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o077,
                0,
                "the secret must not be group/world readable"
            );
        }

        // A truncated file is not half a secret, it is not a secret. Adopting
        // one would put this peer on a mesh nobody else is on, which looks like
        // a share that simply has no other peers.
        std::fs::write(sidecar.join(ORIGIN_SECRET), b"short").expect("truncate");
        assert_eq!(origin_secret_for(&root), None);

        let _ = std::fs::remove_dir_all(&sidecar);
    }

    #[test]
    fn an_empty_filter_takes_everything() {
        assert!(wanted(&[], "a.txt"));
        assert!(wanted(&[], "docs/deep/guide.md"));
    }

    #[test]
    fn a_filter_takes_the_named_file_and_nothing_else() {
        let only = vec!["docs/big.bin".to_owned()];
        assert!(wanted(&only, "docs/big.bin"));
        assert!(!wanted(&only, "a.txt"));
        // A prefix that is not a path boundary must not match, or `--only docs`
        // would quietly take `docsbackup/` too.
        assert!(!wanted(&only, "docs/big.bin.bak"));
    }

    #[test]
    fn naming_a_directory_takes_what_is_under_it() {
        let only = vec!["docs".to_owned()];
        assert!(wanted(&only, "docs/guide.md"));
        assert!(wanted(&only, "docs/deep/nested.md"));
        assert!(!wanted(&only, "docsbackup/guide.md"));
        assert!(!wanted(&only, "a.txt"));
    }

    #[test]
    fn an_ordinary_path_joins_under_the_destination() {
        let dest = Path::new("/tmp/copy");
        assert_eq!(
            safe_join(dest, "docs/guide.md").expect("join"),
            Path::new("/tmp/copy/docs/guide.md")
        );
    }

    /// A manifest comes off the network, so an escaping path is an attack.
    #[test]
    fn a_path_that_escapes_the_destination_is_refused() {
        let dest = Path::new("/tmp/copy");
        for hostile in [
            "../outside",
            "docs/../../outside",
            "..",
            "docs/../../../etc/passwd",
        ] {
            assert!(
                safe_join(dest, hostile).is_err(),
                "{hostile:?} must be refused"
            );
        }
    }

    #[test]
    fn a_backslash_or_nul_is_refused() {
        let dest = Path::new("/tmp/copy");
        assert!(safe_join(dest, "docs\\..\\outside").is_err());
        assert!(safe_join(dest, "docs/gui\0de.md").is_err());
    }

    /// The store must not land inside the directory the user is about to serve,
    /// or `agent-share serve` would publish this machine's bookkeeping.
    #[test]
    fn the_sidecar_store_sits_beside_the_copy_not_inside_it() {
        let dest = Path::new("/tmp/copy");
        let sidecar = sidecar_dir(dest);
        assert!(
            !sidecar.starts_with(dest),
            "the store must not be served as part of the share: {}",
            sidecar.display()
        );
        assert_eq!(sidecar, Path::new("/tmp/.copy.agent-share"));
    }
}
