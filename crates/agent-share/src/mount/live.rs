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

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{Context, Result};
use notify::{RecursiveMode, Watcher as _};
use tokio::sync::broadcast;

use super::{MAX_DELTA_BYTES, WATCH_FRAME_DELTA, WATCH_FRAME_MANIFEST};
use agent_share_proto::manifest::{DirEntry, FileEntry, ManifestDelta, MountManifest};

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

/// The shared tree, and the channel every watcher listens on.
///
/// `pub` only to be re-exported through `crate::test_support`: the enclosing
/// module is `pub(crate)`, so this stays unreachable from outside the crate
/// except by that one deliberate door.
pub struct LiveTree {
    root: PathBuf,
    state: RwLock<TreeState>,
    updates: broadcast::Sender<Arc<Vec<u8>>>,
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
    /// Seed the tree from the startup scan. `paths` is index-aligned with
    /// `manifest.files`, as [`super::scan::scan`] returns them.
    pub(super) fn new(root: PathBuf, manifest: MountManifest, paths: Vec<PathBuf>) -> Self {
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
        let (updates, _) = broadcast::channel(UPDATE_BACKLOG);
        Self {
            root,
            state: RwLock::new(TreeState {
                dirs: manifest.dirs,
                files: manifest.files,
                served,
                index_of,
                encoded,
            }),
            updates,
        }
    }

    /// Seed a tree that **re-serves somebody else's manifest**.
    ///
    /// A mirror is not a second origin. It serves the origin's manifest bytes
    /// *verbatim*, so every index means what the origin says it means, and a
    /// consumer can move between them without re-reading anything. Re-deriving
    /// the manifest from what happens to be on this disk would renumber every
    /// slot after the first gap — and a reader still holding an old index would
    /// then silently get a different file. That is the failure this module
    /// exists to prevent, so the whole point is *not* to scan.
    ///
    /// A slot maps to a local path only when this peer actually has that file,
    /// at the size the origin published. Everything else is `None`, which reads
    /// as `BadIndex`: **partial mirrors are ordinary**, and saying "I do not
    /// have that" is the honest answer. Anything laxer would serve a truncated
    /// or stale file under the origin's name.
    ///
    /// `root` is where the copy lives; `origin_bytes` is exactly what
    /// `OP_MANIFEST` returned from the origin.
    ///
    /// # Errors
    /// `origin_bytes` does not decode as a manifest.
    pub(super) fn mirrored(root: PathBuf, origin_bytes: Vec<u8>) -> Result<Self> {
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
                // catches a half-written mirror. `fofoca-blobs` holds the other
                // half for content this peer can prove.
                match std::fs::metadata(&path) {
                    Ok(meta) if meta.len() == file.size => Some(path),
                    _ => None,
                }
            })
            .collect();

        let (updates, _) = broadcast::channel(UPDATE_BACKLOG);
        Ok(Self {
            root,
            state: RwLock::new(TreeState {
                dirs: manifest.dirs,
                files: manifest.files,
                served,
                index_of,
                // The origin's bytes, untouched. Re-encoding would be a
                // different fingerprint for the same tree, and guard #1 reads
                // that as two peers on different trees.
                encoded: Arc::new(origin_bytes),
            }),
            updates,
        })
    }

    /// How many slots this tree can actually serve, and how many exist.
    ///
    /// `(held, total)`, counting live slots only. Equal for an origin; a
    /// partial mirror holds fewer.
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
    /// `"*"` for a complete tree, sorted ranges for a partial mirror, `None`
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

    /// The encoded manifest as it stands.
    pub(super) fn manifest_bytes(&self) -> Arc<Vec<u8>> {
        Arc::clone(&self.read().encoded)
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
    pub(super) fn opening_frame(&self) -> Vec<u8> {
        Self::manifest_frame(&self.manifest_bytes())
    }

    fn manifest_frame(encoded: &[u8]) -> Vec<u8> {
        let mut frame = Vec::with_capacity(encoded.len() + 1);
        frame.push(WATCH_FRAME_MANIFEST);
        frame.extend_from_slice(encoded);
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

        // Re-encode once per change batch, not per request.
        let refreshed = MountManifest {
            dirs: state.dirs.clone(),
            files: state.files.clone(),
        }
        .encode();
        state.encoded = Arc::new(refreshed);

        let body = delta.encode();
        if u32::try_from(body.len()).is_ok_and(|len| len <= MAX_DELTA_BYTES) {
            let mut frame = Vec::with_capacity(body.len() + 1);
            frame.push(WATCH_FRAME_DELTA);
            frame.extend_from_slice(&body);
            Some(frame)
        } else {
            // Too much changed to describe as a difference. Saying so with the
            // whole manifest is both smaller and simpler than splitting it.
            Some(Self::manifest_frame(&state.encoded))
        }
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
                tracing::debug!(bytes = frame.len(), "publishing a tree change");
                // `Err` only means nobody is watching, which is the common case.
                let _ = tree.updates.send(Arc::new(frame));
            }
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::LiveTree;
    use agent_share_proto::manifest::{FileEntry, ManifestDelta, MountManifest};
    use std::path::PathBuf;

    /// A throwaway directory holding a partial copy of a share.
    struct TempTree(PathBuf);

    impl TempTree {
        fn new(files: &[(&str, usize)]) -> Self {
            use rand::RngCore as _;
            let root =
                std::env::temp_dir().join(format!("agent-share-mirror-{}", rand::rng().next_u64()));
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
    fn a_partial_mirror_keeps_the_origins_indices() {
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

        let tree = LiveTree::mirrored(copy.0.clone(), bytes.clone()).expect("mirrored");

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
        let tree = LiveTree::mirrored(copy.0.clone(), origin.encode()).expect("mirrored");

        assert_eq!(
            tree.path_of(0),
            None,
            "a file at the wrong size must read as absent, not be served short"
        );
        assert_eq!(tree.coverage(), (0, 1));
    }

    #[test]
    fn a_complete_mirror_holds_everything() {
        let origin = MountManifest {
            dirs: Vec::new(),
            files: vec![entry("a.txt", 5), entry("b.txt", 9)],
        };
        let copy = TempTree::new(&[("a.txt", 5), ("b.txt", 9)]);
        let tree = LiveTree::mirrored(copy.0.clone(), origin.encode()).expect("mirrored");
        assert_eq!(tree.coverage(), (2, 2));
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
        ManifestDelta::decode(&frame[1..]).expect("decode the delta")
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
