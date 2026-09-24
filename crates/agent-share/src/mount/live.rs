//! The producer's live view of the shared tree: rescan when the filesystem
//! changes, and hand every watcher the difference.
//!
//! # Why indices are append-only
//!
//! A file's position in the manifest is the address READs use, so the one
//! thing this module may never do is let an index change meaning. A consumer
//! holding index 42 from an older frame and reading it after a compaction
//! would get another file's bytes with an `Ok` status — a corrupt download,
//! not an error anyone could catch.
//!
//! So slots are assigned once and kept: a removed file leaves a tombstone
//! (see [`FileEntry::is_tombstone`]), and a path that comes back reclaims the
//! index it had before. The cost is that a directory churning through
//! filenames grows the table forever; the alternative is silent corruption,
//! which is not a trade.
//!
//! Directories carry no such constraint — nothing addresses them by position
//! — so they are simply replaced wholesale on each scan and diffed by path.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use agent_share_proto::authorship::SecretKey;
use agent_share_proto::authorship::{SIGNATURE_LEN, SignedManifest};
use agent_share_proto::manifest::{DirEntry, FileEntry, ManifestDelta, MountManifest};
use anyhow::{Context, Result};
use notify::{RecursiveMode, Watcher as _};
use tokio::sync::broadcast;

use super::{MAX_DELTA_BYTES, WATCH_FRAME_DELTA, WATCH_FRAME_MANIFEST};

/// How long the tree must sit still before a rescan.
///
/// Editors do not write files, they write temp files and rename them, and a
/// build touches thousands of paths in a burst. Rescanning per event would
/// spend the whole budget re-walking a tree mid-change and publish a torn
/// view of it; waiting for quiet publishes one coherent delta instead.
const DEBOUNCE: Duration = Duration::from_millis(300);

/// How many frames a watcher may fall behind before the producer gives up on
/// catching it up incrementally and resends the whole manifest.
///
/// Deltas only make sense applied in order and without gaps, so a lagging
/// consumer cannot simply skip ahead. Small on purpose: past this, resending
/// is both cheaper and the only correct answer.
const UPDATE_BACKLOG: usize = 64;

/// How many bytes of published deltas an origin keeps so a returning consumer
/// can be told the difference rather than the tree.
///
/// A budget in bytes rather than a count of versions, because bytes are what the
/// process actually spends: a hundred one-file touches cost almost nothing, and
/// four large rescans cost the cap on their own. Past it the oldest are dropped
/// and a consumer asking from that far back is told to take the whole manifest —
/// which by then is the smaller answer anyway.
const DELTA_HISTORY_BYTES: usize = 4 * 1024 * 1024;

/// The shared tree, and the channel every watcher listens on.
///
/// `pub` only to be re-exported through `crate::test_support`: the enclosing
/// module is `pub(crate)`, so this stays unreachable from outside the crate
/// except by that one deliberate door.
pub struct LiveTree {
    root: PathBuf,
    /// The creator's authorship key, held only by an origin that owns this
    /// share. `None` for a seed, which re-serves somebody else's signature
    /// and must not be able to mint one of its own — see
    /// [`agent_share_proto::authorship`].
    author: Option<SecretKey>,
    state: RwLock<TreeState>,
    updates: broadcast::Sender<Arc<Vec<u8>>>,
}

/// One published change, kept so it can be replayed to a consumer that missed it.
struct Published {
    /// The version this delta *arrives at*, so a consumer holding `version - 1`
    /// is the one it applies to.
    version: u64,
    /// The encoded [`ManifestDelta`], exactly as the watch stream carried it.
    delta: Arc<Vec<u8>>,
}

struct TreeState {
    /// Replaced wholesale each scan; consumers key these by path.
    dirs: Vec<DirEntry>,
    /// Append-only, tombstoned in place. Index **is** the READ address.
    files: Vec<FileEntry>,
    /// The absolute path behind each slot, index-aligned with `files`;
    /// `None` wherever `files` holds a tombstone.
    served: Vec<Option<PathBuf>>,
    /// Every path ever assigned a slot, tombstoned ones included, so a
    /// recreated file reclaims its original index instead of taking a new one.
    index_of: HashMap<String, u32>,
    /// The encoded manifest, kept ready so serving one is a clone of an `Arc`
    /// rather than a re-encode of the whole tree per request.
    encoded: Arc<Vec<u8>>,
    /// Bumped on every published change, and signed *inside* the envelope, so
    /// replaying an older manifest the creator really did sign loses to the
    /// newer one a consumer has already seen.
    version: u64,
    /// `version ‖ signature ‖ encoded`, cached beside `encoded` for the same
    /// reason: `OP_MANIFEST` is answered with an `Arc` clone, never a re-sign.
    /// Signing per request would put an ed25519 operation over several MB on
    /// the path of every consumer that connects.
    envelope: Arc<Vec<u8>>,
    /// The creator's signature over `version ‖ encoded`, kept beside the
    /// envelope that carries it. Both the delta frame and `OP_MANIFEST_SINCE`
    /// need it alone, and decoding the envelope to reach it copies the whole
    /// manifest to read sixty-four bytes.
    signature: [u8; SIGNATURE_LEN],
    /// Recent published deltas, oldest first, bounded by
    /// [`DELTA_HISTORY_BYTES`]. What `OP_MANIFEST_SINCE` replays.
    ///
    /// Only deltas land here. A change too large to express as one is published
    /// as a whole manifest instead, and there is nothing to replay for it — the
    /// history is cleared, so a consumer asking across that point is correctly
    /// told to take the tree.
    history: VecDeque<Published>,
    /// Bytes held in `history`, tracked rather than recomputed per push.
    history_bytes: usize,
}

/// Wrap manifest bytes in the envelope `OP_MANIFEST` serves.
///
/// An unsigned share sends a zero signature rather than a shorter body: one
/// wire shape means a reader decides whether to verify from the *ticket*, which
/// it trusts, instead of from the answer, which it does not.
fn seal(
    author: Option<&SecretKey>,
    version: u64,
    encoded: &[u8],
) -> (Arc<Vec<u8>>, [u8; SIGNATURE_LEN]) {
    let signed = agent_share_proto::authorship::sealed(author, version, encoded);
    let signature = signed.signature;
    (Arc::new(signed.encode()), signature)
}

impl std::fmt::Debug for LiveTree {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LiveTree")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl LiveTree {
    /// Seed the tree from the startup scan, unsigned.
    ///
    /// Kept for the tests and for any producer with no authorship key; a real
    /// `serve` goes through [`Self::authored`].
    pub(super) fn new(root: PathBuf, manifest: MountManifest, paths: Vec<PathBuf>) -> Self {
        Self::authored(root, manifest, paths, None)
    }

    /// Seed the tree from the startup scan. `paths` is index-aligned with
    /// `manifest.files`, as [`super::scan::scan`] returns them.
    ///
    /// `author` is the creator's signing key. Version numbering starts at 1
    /// rather than 0 so "never published" and "published once" are different
    /// numbers on the consumer's side.
    pub(super) fn authored(
        root: PathBuf,
        manifest: MountManifest,
        paths: Vec<PathBuf>,
        author: Option<SecretKey>,
    ) -> Self {
        let index_of = manifest
            .files
            .iter()
            .enumerate()
            .map(|(position, file)| {
                (
                    file.rel_path.clone(),
                    u32::try_from(position).expect("file count fits u32"),
                )
            })
            .collect();
        let served = paths.into_iter().map(Some).collect();
        let encoded = Arc::new(manifest.encode());
        let (envelope, signature) = seal(author.as_ref(), 1, &encoded);
        let (updates, _) = broadcast::channel(UPDATE_BACKLOG);
        Self {
            root,
            author,
            state: RwLock::new(TreeState {
                dirs: manifest.dirs,
                files: manifest.files,
                served,
                index_of,
                encoded,
                version: 1,
                envelope,
                signature,
                history: VecDeque::new(),
                history_bytes: 0,
            }),
            updates,
        }
    }

    /// Seed a tree that **re-serves somebody else's manifest**.
    ///
    /// A seed is not a second origin. It serves the origin's manifest bytes
    /// *verbatim*, so every index means what the origin says it means, and a
    /// consumer can move between them without re-reading anything. Re-deriving
    /// the manifest from what happens to be on this disk would renumber every
    /// slot after the first gap — and a reader still holding an old index would
    /// then silently get a different file. That is the failure this module
    /// exists to prevent, so the whole point is *not* to scan.
    ///
    /// A slot maps to a local path only when this peer actually has that file,
    /// at the size the origin published. Everything else is `None`, which reads
    /// as `BadIndex`: **partial seeds are ordinary**, and saying "I do not
    /// have that" is the honest answer. Anything laxer would serve a truncated
    /// or stale file under the origin's name.
    ///
    /// `root` is where the copy lives; `envelope` is exactly what `OP_MANIFEST`
    /// returned from the origin, signature included.
    ///
    /// **The signature is re-served, never re-made.** A seed holds no
    /// authorship key by design, so the only proof it can offer is the one the
    /// creator already published — which is enough, because a signature says
    /// who wrote the bytes and not who handed them over.
    ///
    /// # Errors
    /// `envelope` does not decode, or its manifest does not.
    pub(super) fn seeded(root: PathBuf, envelope: Vec<u8>) -> Result<Self> {
        let origin_bytes = SignedManifest::decode(&envelope)?.manifest;
        let manifest = MountManifest::decode(&origin_bytes)?;
        let index_of = manifest
            .files
            .iter()
            .enumerate()
            .map(|(position, file)| {
                (
                    file.rel_path.clone(),
                    u32::try_from(position).expect("file count fits u32"),
                )
            })
            .collect();

        // Aligned to the origin's `files` by position, not by what a directory
        // walk would find. A gap stays a gap.
        let served = manifest
            .files
            .iter()
            .map(|file| {
                if file.is_tombstone() {
                    return None;
                }
                let path = root.join(file.rel_path.replace('/', std::path::MAIN_SEPARATOR_STR));
                // Size is the cheap half of the version gate, and the half that
                // catches a half-written seed. `fofoca-blobs` holds the other
                // half for content this peer can prove.
                match std::fs::metadata(&path) {
                    Ok(meta) if meta.len() == file.size => Some(path),
                    _ => None,
                }
            })
            .collect();

        let (updates, _) = broadcast::channel(UPDATE_BACKLOG);
        // Both off one decode: a seed re-serves the creator's signature
        // verbatim and can mint none of its own.
        let origin = SignedManifest::decode(&envelope)?;
        let (version, signature) = (origin.version, origin.signature);
        Ok(Self {
            root,
            author: None,
            state: RwLock::new(TreeState {
                dirs: manifest.dirs,
                files: manifest.files,
                served,
                index_of,
                // The origin's bytes, untouched. Re-encoding would be a
                // different fingerprint for the same tree, and guard #1 reads
                // that as two peers on different trees.
                encoded: Arc::new(origin_bytes),
                version,
                envelope: Arc::new(envelope),
                signature,
                // A seed publishes no changes of its own, so it never has a
                // difference to replay — see `LiveTree::seeded`.
                history: VecDeque::new(),
                history_bytes: 0,
            }),
            updates,
        })
    }

    /// How many slots this tree can actually serve, and how many exist.
    ///
    /// `(held, total)`, counting live slots only. Equal for an origin; a
    /// partial seed holds fewer.
    pub(super) fn coverage(&self) -> (usize, usize) {
        let state = self.read();
        let total = state
            .files
            .iter()
            .filter(|file| !file.is_tombstone())
            .count();
        let held = state.served.iter().filter(|slot| slot.is_some()).count();
        (held, total)
    }

    /// Which slots this tree can serve, encoded for a peer card.
    ///
    /// `"*"` for a complete tree, sorted ranges for a partial seed, `None`
    /// when it holds nothing or the answer will not fit a frame — see
    /// [`agent_share_proto::serving`].
    pub(super) fn serving(&self) -> Option<String> {
        let state = self.read();
        let held: Vec<u32> = state
            .served
            .iter()
            .enumerate()
            .filter_map(|(slot, path)| {
                path.as_ref()?;
                u32::try_from(slot).ok()
            })
            .collect();
        let total = state
            .files
            .iter()
            .filter(|file| !file.is_tombstone())
            .count();
        agent_share_proto::serving::encode_serving(&held, total)
    }

    /// The encoded manifest as it stands, without the envelope around it.
    ///
    /// This is the fingerprint domain — `manifest_fingerprint` is defined over
    /// exactly these bytes — so it deliberately excludes the version and
    /// signature. Two peers on the same tree agree here whether or not either
    /// of them can prove who wrote it.
    pub(super) fn manifest_bytes(&self) -> Arc<Vec<u8>> {
        Arc::clone(&self.read().encoded)
    }

    /// What `OP_MANIFEST` answers with: `version ‖ signature ‖ manifest`.
    pub(super) fn manifest_envelope(&self) -> Arc<Vec<u8>> {
        Arc::clone(&self.read().envelope)
    }

    /// The absolute path behind a READ index, or `None` for an index that is
    /// out of range or tombstoned — both of which answer `BadIndex`.
    ///
    /// Returns an owned path so the caller can do its I/O without holding the
    /// lock, which it must not do across an await anyway.
    pub(super) fn path_of(&self, index: u32) -> Option<PathBuf> {
        let state = self.read();
        let slot = usize::try_from(index).ok()?;
        state.served.get(slot)?.clone()
    }

    /// Subscribe to change frames. The caller writes
    /// [`Self::opening_frame`] first, then everything this yields.
    pub(super) fn subscribe(&self) -> broadcast::Receiver<Arc<Vec<u8>>> {
        self.updates.subscribe()
    }

    /// The full-manifest frame a watcher opens with, and the one it is resent
    /// after falling behind.
    ///
    /// **Watch frames carry the signed envelope**, so a consumer checks who
    /// wrote a change rather than inferring it from who handed it over. That is
    /// what lets a peer follow updates through a seeder: only the creator can
    /// author a version, but anybody may carry one. Deltas travel the same way
    /// — see the [`ManifestSince`] frame beside this one, which is the exact
    /// shape `OP_MANIFEST_SINCE` already answers with, so both are verified by
    /// the same `apply_since` on the far side.
    pub(super) fn opening_frame(&self) -> Vec<u8> {
        Self::manifest_frame(&self.manifest_envelope())
    }

    fn manifest_frame(envelope: &[u8]) -> Vec<u8> {
        let mut frame = Vec::with_capacity(envelope.len() + 1);
        frame.push(WATCH_FRAME_MANIFEST);
        frame.extend_from_slice(envelope);
        frame
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, TreeState> {
        self.state.read().unwrap_or_else(|poisoned| {
            // A panic under the write lock would leave the tree half-updated.
            // Reading it anyway is worse than stopping.
            panic!("the live tree lock is poisoned: {poisoned}")
        })
    }

    /// Walk the tree again and fold the result in, returning the frame to
    /// broadcast, or `None` when nothing changed.
    fn apply(&self, manifest: MountManifest, paths: Vec<PathBuf>) -> Option<Vec<u8>> {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(|poisoned| panic!("the live tree lock is poisoned: {poisoned}"));
        let mut delta = ManifestDelta::default();

        // Directories: diff by path, then take the fresh list as-is.
        {
            let previous: HashMap<&str, (u32, i64)> = state
                .dirs
                .iter()
                .map(|dir| (dir.rel_path.as_str(), (dir.mode, dir.mtime)))
                .collect();
            for dir in &manifest.dirs {
                if previous.get(dir.rel_path.as_str()) != Some(&(dir.mode, dir.mtime)) {
                    delta.dirs_upserted.push(dir.clone());
                }
            }
            let fresh: HashSet<&str> = manifest
                .dirs
                .iter()
                .map(|dir| dir.rel_path.as_str())
                .collect();
            for dir in &state.dirs {
                if !fresh.contains(dir.rel_path.as_str()) {
                    delta.dirs_removed.push(dir.rel_path.clone());
                }
            }
        }
        state.dirs = manifest.dirs;

        // Files: upsert into the slot this path already owns, or append.
        let mut live_slots: HashSet<u32> = HashSet::with_capacity(manifest.files.len());
        for (entry, abs) in manifest.files.into_iter().zip(paths) {
            if let Some(index) = state.index_of.get(&entry.rel_path).copied() {
                live_slots.insert(index);
                let slot = usize::try_from(index).expect("u32 fits usize");
                let previous = &state.files[slot];
                let unchanged = previous.size == entry.size
                    && previous.mode == entry.mode
                    && previous.mtime == entry.mtime
                    && !previous.is_tombstone();
                if unchanged {
                    continue;
                }
                state.served[slot] = Some(abs);
                state.files[slot] = entry.clone();
                delta.files_upserted.push((index, entry));
            } else {
                let index = u32::try_from(state.files.len()).expect("file count fits u32");
                live_slots.insert(index);
                state.index_of.insert(entry.rel_path.clone(), index);
                state.served.push(Some(abs));
                state.files.push(entry.clone());
                delta.files_upserted.push((index, entry));
            }
        }

        // Whatever the scan did not account for is gone. Collect first: the
        // scan above holds an immutable borrow the tombstoning would clash
        // with.
        let vanished: Vec<u32> = state
            .files
            .iter()
            .enumerate()
            .filter(|(position, file)| {
                !file.is_tombstone()
                    && !live_slots.contains(&u32::try_from(*position).expect("fits u32"))
            })
            .map(|(position, _)| u32::try_from(position).expect("fits u32"))
            .collect();
        for index in &vanished {
            let slot = usize::try_from(*index).expect("u32 fits usize");
            state.files[slot] = FileEntry::tombstone();
            state.served[slot] = None;
        }
        delta.files_removed = vanished;

        if delta.is_empty() {
            return None;
        }

        // Re-encode and re-sign once per change batch, not per request.
        let refreshed = MountManifest {
            dirs: state.dirs.clone(),
            files: state.files.clone(),
        }
        .encode();
        state.encoded = Arc::new(refreshed);
        state.version += 1;
        (state.envelope, state.signature) =
            seal(self.author.as_ref(), state.version, &state.encoded);

        let body = delta.encode();
        if u32::try_from(body.len()).is_ok_and(|len| len <= MAX_DELTA_BYTES) {
            // Carried as a `ManifestSince` — the same shape `OP_MANIFEST_SINCE`
            // answers with — so the version and the creator's signature travel
            // with the difference and the consumer verifies its reconstruction
            // through the one `apply_since` both paths already share.
            let since = agent_share_proto::framing::ManifestSince {
                target_version: state.version,
                signature: state.signature,
                deltas: vec![body.clone()],
            }
            .encode();
            let mut frame = Vec::with_capacity(since.len() + 1);
            frame.push(WATCH_FRAME_DELTA);
            frame.extend_from_slice(&since);
            state.remember(body);
            Some(frame)
        } else {
            // Too much changed to describe as a difference. Saying so with the
            // whole manifest is both smaller and simpler than splitting it.
            //
            // And the history goes with it: a consumer cannot cross this point
            // by applying deltas, because there is no delta describing it. Kept
            // entries would let a later request span the gap and reconstruct a
            // tree that never existed — the signature check on the far side
            // would catch it, but answering with a chain we know is broken is
            // not something to leave for the reader to catch.
            state.forget_history();
            Some(Self::manifest_frame(&state.envelope))
        }
    }

    /// Deltas carrying a consumer at `since` up to the current version.
    ///
    /// `None` when that cannot be done — the version is unknown, older than the
    /// history reaches, or newer than this tree — and the caller answers with
    /// the whole manifest instead. `Some` with an empty chain means "you are
    /// already current", which is the common answer and the cheap one.
    ///
    /// The signature returned is the **current** version's, over the manifest a
    /// correct consumer will have reconstructed. That is what makes an unsigned
    /// delta safe to send; see `OP_MANIFEST_SINCE`.
    pub(super) fn deltas_since(
        &self,
        since: u64,
    ) -> Option<(u64, [u8; SIGNATURE_LEN], Vec<Vec<u8>>)> {
        let state = self.read();
        if since > state.version {
            // Ahead of us: a consumer holding a version this tree never
            // published, which a restarted origin produces. Nothing to replay.
            return None;
        }
        let signature = state.signature;
        if since == state.version {
            return Some((state.version, signature, Vec::new()));
        }
        // Every step from `since + 1` to now has to be present. A gap cannot be
        // skipped: deltas apply in order, against exactly the state before them.
        let mut chain = Vec::new();
        let mut want = since + 1;
        for entry in &state.history {
            if entry.version < want {
                continue;
            }
            if entry.version != want {
                return None;
            }
            chain.push(entry.delta.as_ref().clone());
            want += 1;
        }
        (want > state.version).then_some((state.version, signature, chain))
    }
}

impl TreeState {
    /// Keep `delta` as the step that reached the current version.
    fn remember(&mut self, delta: Vec<u8>) {
        self.history_bytes += delta.len();
        self.history.push_back(Published {
            version: self.version,
            delta: Arc::new(delta),
        });
        while self.history_bytes > DELTA_HISTORY_BYTES {
            let Some(oldest) = self.history.pop_front() else {
                break;
            };
            self.history_bytes -= oldest.delta.len();
        }
    }

    fn forget_history(&mut self) {
        self.history.clear();
        self.history_bytes = 0;
    }
}

/// Rescan on change until the process ends.
///
/// # Errors
/// The watcher cannot be created, or `root` cannot be watched.
pub(super) fn spawn_watcher(tree: Arc<LiveTree>) -> Result<()> {
    // `notify` calls back on its own thread, so the bridge into async has to
    // be a sender that works off-runtime. Unbounded because dropping an event
    // means missing a change; the debounce below is what bounds the work.
    let (events, mut incoming) = tokio::sync::mpsc::unbounded_channel();
    let mut watcher = notify::recommended_watcher(move |event| {
        let _ = events.send(event);
    })
    .context("creating the filesystem watcher")?;
    watcher
        .watch(&tree.root, RecursiveMode::Recursive)
        .with_context(|| format!("watching {}", tree.root.display()))?;

    tokio::spawn(async move {
        // Dropping the watcher stops the events, so it lives as long as the
        // loop that reads them.
        let _watcher = watcher;
        while incoming.recv().await.is_some() {
            // Wait for the tree to go quiet rather than rescanning into the
            // middle of a burst.
            loop {
                tokio::time::sleep(DEBOUNCE).await;
                let mut more = false;
                while incoming.try_recv().is_ok() {
                    more = true;
                }
                if !more {
                    break;
                }
            }
            let root = tree.root.clone();
            // `scan` is blocking and can walk a very large tree; keeping it
            // off the runtime's worker threads matters on a 70k-file share.
            let scanned = tokio::task::spawn_blocking(move || super::scan::scan(&root)).await;
            let scanned = match scanned {
                Ok(Ok(scanned)) => scanned,
                Ok(Err(error)) => {
                    tracing::warn!(%error, "rescan failed; serving the previous tree");
                    continue;
                }
                Err(error) => {
                    tracing::warn!(%error, "rescan task failed; serving the previous tree");
                    continue;
                }
            };
            let (manifest, paths) = scanned;
            if let Some(frame) = tree.apply(manifest, paths) {
                let bytes = frame.len();
                // `Err` only means nobody is watching, which is the common case.
                let watchers = tree.updates.send(Arc::new(frame)).unwrap_or(0);
                let (version, files) = {
                    let state = tree.read();
                    (state.version, state.files.len())
                };
                // `info`, and carrying the subscriber count, because these are
                // the two questions asked when a peer looks stale: did the
                // producer notice, and was anyone still listening when it did?
                // A change published to nobody is a very different bug from one
                // that was never published.
                //
                // Both numbers, because `serve` holds a feed of its own to
                // republish its mesh card — so `feeds` is never 0 while a
                // producer is up, and `peers` is the one to read.
                tracing::info!(
                    version,
                    files,
                    bytes,
                    feeds = watchers,
                    peers = watchers.saturating_sub(1),
                    "published a tree change"
                );
            }
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use agent_share_proto::manifest::{FileEntry, ManifestDelta, MountManifest};

    use super::{LiveTree, SIGNATURE_LEN, SecretKey, SignedManifest, seal};

    /// A throwaway directory holding a partial copy of a share.
    struct TempTree(PathBuf);

    impl TempTree {
        fn new(files: &[(&str, usize)]) -> Self {
            use rand::RngCore as _;
            let root =
                std::env::temp_dir().join(format!("agent-share-seed-{}", rand::rng().next_u64()));
            for (name, size) in files {
                let path = root.join(name);
                std::fs::create_dir_all(path.parent().expect("has a parent")).expect("mkdir");
                std::fs::write(&path, vec![7u8; *size]).expect("write");
            }
            std::fs::create_dir_all(&root).expect("mkdir root");
            Self(root)
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// **The re-seeder's whole reason for existing.** A partial copy must serve
    /// the origin's manifest untouched, so slot 2 is still slot 2 even when the
    /// files at slots 0 and 1 were never fetched.
    ///
    /// Deriving the manifest from the directory instead would renumber every
    /// slot after the first gap, and a reader holding an index from the origin
    /// would silently get a different file.
    #[test]
    fn a_partial_seed_keeps_the_origins_indices() {
        // The origin's tree: three files. This peer fetched only the third.
        let origin = MountManifest {
            dirs: Vec::new(),
            files: vec![
                entry("a.txt", 5),
                entry("b.txt", 5),
                entry("docs/big.bin", 64),
            ],
        };
        let bytes = origin.encode();
        let copy = TempTree::new(&[("docs/big.bin", 64)]);

        let tree = LiveTree::seeded(copy.0.clone(), sealed(&bytes)).expect("seeded");

        assert_eq!(
            *tree.manifest_bytes(),
            bytes,
            "the origin's bytes must be re-served verbatim, not re-encoded"
        );
        assert_eq!(tree.path_of(0), None, "a file we never fetched is absent");
        assert_eq!(tree.path_of(1), None);
        assert_eq!(
            tree.path_of(2),
            Some(copy.0.join("docs/big.bin")),
            "the file we do hold is still at the origin's index"
        );
        assert_eq!(tree.coverage(), (1, 3));
    }

    /// A half-written file is not a servable file. Size is the cheap half of
    /// the version gate and the half that catches an interrupted copy.
    #[test]
    fn a_truncated_copy_is_not_served() {
        let origin = MountManifest {
            dirs: Vec::new(),
            files: vec![entry("a.bin", 1000)],
        };
        // On disk at the wrong length: an interrupted fetch.
        let copy = TempTree::new(&[("a.bin", 400)]);
        let tree = LiveTree::seeded(copy.0.clone(), sealed(&origin.encode())).expect("seeded");

        assert_eq!(
            tree.path_of(0),
            None,
            "a file at the wrong size must read as absent, not be served short"
        );
        assert_eq!(tree.coverage(), (0, 1));
    }

    #[test]
    fn a_complete_seed_holds_everything() {
        let origin = MountManifest {
            dirs: Vec::new(),
            files: vec![entry("a.txt", 5), entry("b.txt", 9)],
        };
        let copy = TempTree::new(&[("a.txt", 5), ("b.txt", 9)]);
        let tree = LiveTree::seeded(copy.0.clone(), sealed(&origin.encode())).expect("seeded");
        assert_eq!(tree.coverage(), (2, 2));
    }

    /// The creator, for tests that care who signed.
    fn creator() -> SecretKey {
        SecretKey::from_bytes(&[4u8; 32])
    }

    /// An `OP_MANIFEST` envelope around `bytes`, as an origin would serve it.
    fn sealed(bytes: &[u8]) -> Vec<u8> {
        seal(Some(&creator()), 1, bytes).0.as_ref().clone()
    }

    /// **The requirement, as a test.** A seed is handed the creator's
    /// signature and re-serves it byte for byte; it never makes one, and could
    /// not, because it holds no authorship key.
    #[test]
    fn a_seed_re_serves_the_creators_signature_rather_than_making_one() {
        let origin = MountManifest {
            dirs: Vec::new(),
            files: vec![entry("a.txt", 5)],
        };
        let envelope = sealed(&origin.encode());
        let copy = TempTree::new(&[("a.txt", 5)]);
        let tree = LiveTree::seeded(copy.0.clone(), envelope.clone()).expect("seeded");

        assert_eq!(
            *tree.manifest_envelope(),
            envelope,
            "a copy must hand on the envelope it was given, signature included"
        );
        assert!(
            tree.author.is_none(),
            "a seed holding a signing key would be able to publish"
        );
    }

    /// The version rides inside the signature and moves with the tree, which is
    /// what makes replaying an older manifest useless.
    #[test]
    fn publishing_a_change_signs_a_new_version() {
        let manifest = MountManifest {
            dirs: Vec::new(),
            files: vec![entry("a", 1)],
        };
        let tree = LiveTree::authored(
            PathBuf::from("/root"),
            manifest,
            vec![PathBuf::from("/root/a")],
            Some(creator()),
        );
        let before = SignedManifest::decode(&tree.manifest_envelope()).expect("decode");
        assert_eq!(before.version, 1);
        assert!(before.accept(&creator().public(), 0).is_ok_and(|()| true));

        rescan(&tree, &[("a", 4096)]);
        let after = SignedManifest::decode(&tree.manifest_envelope()).expect("decode");
        assert_eq!(after.version, 2);
        assert!(after.accept(&creator().public(), before.version).is_ok());
        // And the old one loses to the new: replaying it is refused.
        assert!(before.accept(&creator().public(), after.version).is_err());
    }

    /// An unsigned share still has one wire shape. The zero signature must not
    /// pass for anybody's.
    #[test]
    fn an_unsigned_tree_serves_a_zero_signature() {
        let tree = tree(&[("a", 1)]);
        let signed = SignedManifest::decode(&tree.manifest_envelope()).expect("decode");
        assert_eq!(signed.signature, [0u8; SIGNATURE_LEN]);
        assert!(signed.accept(&creator().public(), 0).is_err());
        assert_eq!(
            *tree.manifest_bytes(),
            signed.manifest,
            "the fingerprint domain is the manifest, not the envelope around it"
        );
    }

    fn entry(path: &str, size: u64) -> FileEntry {
        FileEntry {
            rel_path: path.to_owned(),
            size,
            mode: 0o644,
            mtime: 1,
        }
    }

    fn tree(files: &[(&str, u64)]) -> LiveTree {
        let manifest = MountManifest {
            dirs: Vec::new(),
            files: files
                .iter()
                .map(|(path, size)| entry(path, *size))
                .collect(),
        };
        let paths = files
            .iter()
            .map(|(path, _)| PathBuf::from(format!("/root/{path}")))
            .collect();
        LiveTree::new(PathBuf::from("/root"), manifest, paths)
    }

    /// **The end-to-end claim of `OP_MANIFEST_SINCE`**, asserted against the
    /// real producer rather than a hand-built chain: a consumer holding an
    /// earlier version is carried forward by the deltas alone, and what it
    /// rebuilds is byte-for-byte what the creator signed.
    ///
    /// If those two ever diverge the signature check is what catches it, so
    /// this is also the test that keeps the fallback from becoming the only
    /// path — a silently-broken chain would still be *safe*, and permanently
    /// useless.
    #[test]
    fn a_consumer_is_carried_forward_by_the_deltas_alone() {
        let tree = LiveTree::authored(
            PathBuf::from("/root"),
            MountManifest {
                dirs: Vec::new(),
                files: vec![entry("a", 1)],
            },
            vec![PathBuf::from("/root/a")],
            Some(creator()),
        );
        let held = SignedManifest::decode(&tree.manifest_envelope()).expect("decode");

        // Already current: an empty chain, which is the common answer.
        let (version, _, chain) = tree.deltas_since(held.version).expect("current");
        assert_eq!(version, held.version);
        assert!(chain.is_empty());

        rescan(&tree, &[("a", 4096)]);
        rescan(&tree, &[("a", 4096), ("b", 7)]);

        let (target, signature, deltas) = tree.deltas_since(held.version).expect("a difference");
        assert_eq!(deltas.len(), 2, "one per published version");
        let caught_up = agent_share_proto::manifest::apply_since(
            &held,
            &agent_share_proto::framing::ManifestSince {
                target_version: target,
                signature,
                deltas,
            },
            Some(&creator().public()),
        )
        .expect("the chain must rebuild what was signed");
        let current = SignedManifest::decode(&tree.manifest_envelope()).expect("decode");
        assert_eq!(caught_up.version, current.version);
        assert_eq!(caught_up.manifest, current.manifest);
    }

    /// A version the history no longer reaches is refused rather than answered
    /// with a chain that skips a step. Deltas apply against exactly the state
    /// before them, so a gap is not something a consumer could survive.
    #[test]
    fn a_version_past_the_history_is_refused() {
        let tree = LiveTree::authored(
            PathBuf::from("/root"),
            MountManifest {
                dirs: Vec::new(),
                files: vec![entry("a", 1)],
            },
            vec![PathBuf::from("/root/a")],
            Some(creator()),
        );
        rescan(&tree, &[("a", 2)]);

        assert!(
            tree.deltas_since(0).is_none(),
            "version 0 predates the first published delta"
        );
        assert!(
            tree.deltas_since(99).is_none(),
            "a version this tree never published — a restarted origin — is refused"
        );
        assert!(tree.deltas_since(1).is_some(), "the step we do hold");
    }

    fn rescan(tree: &LiveTree, files: &[(&str, u64)]) -> ManifestDelta {
        let manifest = MountManifest {
            dirs: Vec::new(),
            files: files
                .iter()
                .map(|(path, size)| entry(path, *size))
                .collect(),
        };
        let paths = files
            .iter()
            .map(|(path, _)| PathBuf::from(format!("/root/{path}")))
            .collect();
        let frame = tree.apply(manifest, paths).expect("something changed");
        // A delta frame is a `ManifestSince` — version and signature travel
        // with the difference so a consumer can verify what it rebuilds,
        // whoever relayed it. One delta per published change.
        let since = agent_share_proto::framing::ManifestSince::decode(&frame[1..])
            .expect("decode the since frame");
        let [delta] = since.deltas.as_slice() else {
            panic!(
                "one published change carries one delta, got {}",
                since.deltas.len()
            );
        };
        ManifestDelta::decode(delta).expect("decode the delta")
    }

    #[test]
    fn an_unchanged_tree_produces_no_frame() {
        let tree = tree(&[("a", 1), ("b", 2)]);
        let manifest = MountManifest {
            dirs: Vec::new(),
            files: vec![entry("a", 1), entry("b", 2)],
        };
        let paths = vec![PathBuf::from("/root/a"), PathBuf::from("/root/b")];
        assert!(tree.apply(manifest, paths).is_none());
    }

    #[test]
    fn a_removed_file_leaves_its_slot_alone() {
        // The whole point: `b` disappearing must not move `c` from 2 to 1.
        let tree = tree(&[("a", 1), ("b", 2), ("c", 3)]);
        let delta = rescan(&tree, &[("a", 1), ("c", 3)]);
        assert_eq!(delta.files_removed, vec![1]);
        assert!(delta.files_upserted.is_empty(), "nothing else moved");
        assert_eq!(tree.path_of(2), Some(PathBuf::from("/root/c")));
        assert_eq!(tree.path_of(1), None, "a tombstone serves nothing");
    }

    #[test]
    fn a_new_file_appends_rather_than_sorting_in() {
        // `aaa` sorts first, but taking index 0 would renumber everything.
        let tree = tree(&[("m", 1)]);
        let delta = rescan(&tree, &[("aaa", 9), ("m", 1)]);
        assert_eq!(delta.files_upserted, vec![(1, entry("aaa", 9))]);
        assert_eq!(tree.path_of(0), Some(PathBuf::from("/root/m")));
    }

    #[test]
    fn a_recreated_file_reclaims_its_original_index() {
        let tree = tree(&[("a", 1), ("b", 2)]);
        assert_eq!(rescan(&tree, &[("a", 1)]).files_removed, vec![1]);
        let delta = rescan(&tree, &[("a", 1), ("b", 7)]);
        assert_eq!(delta.files_upserted, vec![(1, entry("b", 7))]);
        assert_eq!(tree.path_of(1), Some(PathBuf::from("/root/b")));
    }

    #[test]
    fn a_grown_file_reports_its_new_size() {
        let tree = tree(&[("a", 1)]);
        let delta = rescan(&tree, &[("a", 4096)]);
        assert_eq!(delta.files_upserted, vec![(0, entry("a", 4096))]);
    }

    #[test]
    fn the_served_manifest_keeps_tombstones_in_place() {
        let tree = tree(&[("a", 1), ("b", 2), ("c", 3)]);
        rescan(&tree, &[("a", 1), ("c", 3)]);
        let manifest = MountManifest::decode(&tree.manifest_bytes()).expect("decode");
        assert_eq!(manifest.files.len(), 3, "the slot survives the file");
        assert!(manifest.files[1].is_tombstone());
        assert_eq!(manifest.files[2].rel_path, "c");
    }
}
