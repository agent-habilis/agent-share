//! `agent-share mirror` — take a full copy of a share, and be able to serve it.
//!
//! A separate verb from the lazy mount, deliberately. The mount is diskless: it
//! fetches bytes as a reader touches them and keeps nothing, which is what makes
//! browsing a 500 GB share cheap. A mirror is the opposite trade, taken on
//! purpose — it downloads everything so that this machine can hand it to
//! somebody else. **A peer that holds no bytes cannot seed, and no protocol
//! design removes that.**
//!
//! Its output is an ordinary directory, so the way to seed it is the verb that
//! already exists:
//!
//! ```sh
//! agent-share mirror <ticket> ./copy
//! agent-share serve ./copy          # now a second source for that tree
//! ```
//!
//! # Verifying without a verified transport
//!
//! `OP_READ` returns raw bytes, not a bao stream, so a mirror cannot verify
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

use anyhow::{Context, Result, bail};
use fofoca_blobs::{BlobStore, FileId, FsStore};

use super::MountTicket;
use super::consume::RemoteClient;
use crate::file::human_bytes;
use crate::lookup::{add_peer_addr, build_endpoint};

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
}

/// Copy every file in the share behind `ticket` into `dest`.
///
/// # Errors
/// The ticket does not decode, the share is unreachable, `dest` cannot be
/// written, or a file fails to verify against the root the origin published.
pub(crate) async fn mirror(ticket: &str, dest: &Path, json: bool) -> Result<()> {
    let ticket = MountTicket::decode(ticket)?;
    let endpoint = build_endpoint(&ticket.lookups, None, None, Vec::new(), None, false).await?;
    add_peer_addr(&endpoint, ticket.addr.clone())?;
    let client = RemoteClient::new(endpoint.clone(), ticket);

    let manifest = client.fetch_manifest().await?;
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
    let store = FsStore::open(sidecar_dir(dest)).context("opening the mirror's hash store")?;

    let mut tally = Tally::default();
    for (index, file) in manifest.files.iter().enumerate() {
        // Tombstones hold a slot open so later indices keep meaning what they
        // meant; there is no file to copy.
        if file.is_tombstone() {
            continue;
        }
        let index = u32::try_from(index).context("manifest index fits u32")?;
        let path = safe_join(dest, &file.rel_path)?;
        if !json {
            crate::util::output::status("Fetching", &file.rel_path);
        }
        let bytes = fetch_whole(&client, index, file.size).await?;
        write_file(&path, &bytes)?;
        let verified = record(&store, &client, index, &path, &bytes).await?;

        tally.files += 1;
        tally.bytes += bytes.len() as u64;
        if verified {
            tally.verified += 1;
        } else {
            tally.unverified += 1;
        }
    }

    endpoint.close().await;
    report(&tally, dest, json);
    Ok(())
}

/// Where a mirror keeps what it knows about the copy.
///
/// A sibling of the destination rather than a child, because the destination is
/// meant to be handed straight to `agent-share serve` and anything inside it
/// would be served as part of the share.
fn sidecar_dir(dest: &Path) -> PathBuf {
    let name = dest.file_name().map_or_else(
        || "mirror".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    dest.parent()
        .unwrap_or(Path::new("."))
        .join(format!(".{name}.agent-share"))
}

/// Read a whole file over `OP_READ`, one capped chunk at a time.
async fn fetch_whole(client: &RemoteClient, index: u32, size: u64) -> Result<Vec<u8>> {
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

fn write_file(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))
}

/// Hash the copy, cross-check it against the origin, and record the binding.
///
/// Returns whether the origin was able to vouch for the content *and* agreed.
async fn record(
    store: &FsStore,
    client: &RemoteClient,
    index: u32,
    path: &Path,
    bytes: &[u8],
) -> Result<bool> {
    let meta = std::fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
    let file = FileId {
        key: path.to_string_lossy().into_owned(),
        size: meta.len(),
        mtime: meta
            .modified()
            .ok()
            .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |since| since.as_secs().cast_signed()),
    };
    let ours = store.insert_complete(&file, bytes).await?;

    // `None` is ordinary — the origin keeps no hash cache, or cannot vouch for
    // that index. The copy stands; it simply is not provable from here.
    let Some((theirs, _)) = client.fetch_hash(index).await? else {
        return Ok(false);
    };
    if ours != theirs {
        bail!(
            "{} does not match the origin: the bytes were altered in transit, \
             or the origin is serving content it did not hash",
            path.display()
        );
    }
    Ok(true)
}

/// Join `rel` under `dest`, refusing anything that would escape it.
///
/// A manifest arrives over the network, so `../..` in a path is an attack, not
/// a typo. `nfs.rs` rejects the same shapes on the mount side; a mirror writes
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

fn report(tally: &Tally, dest: &Path, json: bool) {
    if json {
        println!("agent-share serve {}", dest.display());
        return;
    }
    crate::util::output::status_out(
        "Mirrored",
        &format!(
            "{} files, {} — {} verified against the origin, {} unverified",
            tally.files,
            human_bytes(tally.bytes),
            tally.verified,
            tally.unverified
        ),
    );
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
    use super::{safe_join, sidecar_dir};
    use std::path::Path;

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
