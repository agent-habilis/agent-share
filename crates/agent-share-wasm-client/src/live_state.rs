//! Index-stable live tree for the browser producer.
//!
//! Port of native `LiveTree::apply` without tokio/notify/PathBuf. Generic over
//! the per-slot payload so host unit tests can use `T = ()`.

use std::collections::{HashMap, HashSet};

use agent_share_proto::manifest::{DirEntry, FileEntry, MountManifest};

/// Append-only file table with tombstones; directories replaced wholesale.
pub(crate) struct LiveState<T> {
    dirs: Vec<DirEntry>,
    /// Wire order; tombstones stay in place so READ indices never shift.
    files: Vec<FileEntry>,
    /// Index-aligned with `files`; `None` where tombstoned.
    slots: Vec<Option<T>>,
    /// Every path ever assigned a slot, including tombstoned ones.
    index_of: HashMap<String, u32>,
    /// Re-encoded once per change batch.
    encoded: Vec<u8>,
    /// Bumped once per change batch, exactly as the native `LiveTree` does.
    ///
    /// Starts at 1, not 0, so "never published" and "published once" are
    /// different numbers on the consumer's side — the same reason native gives.
    /// It is inside the signature, so it is also what refuses a rollback.
    version: u64,
}

impl<T> LiveState<T> {
    pub fn new(dirs: Vec<DirEntry>, files: Vec<(FileEntry, T)>) -> Self {
        let mut index_of = HashMap::with_capacity(files.len());
        let mut file_entries = Vec::with_capacity(files.len());
        let mut slots = Vec::with_capacity(files.len());
        for (position, (entry, payload)) in files.into_iter().enumerate() {
            let index = u32::try_from(position).expect("file count fits u32");
            index_of.insert(entry.rel_path.clone(), index);
            file_entries.push(entry);
            slots.push(Some(payload));
        }
        let encoded = MountManifest {
            dirs: dirs.clone(),
            files: file_entries.clone(),
        }
        .encode();
        Self {
            dirs,
            files: file_entries,
            slots,
            index_of,
            encoded,
            version: 1,
        }
    }

    /// Fold a rescan in. Returns true when anything changed.
    ///
    /// Duplicate `rel_path`s in `files` are first-wins; later duplicates are
    /// dropped before the fold.
    pub fn apply(&mut self, dirs: Vec<DirEntry>, files: Vec<(FileEntry, T)>) -> bool {
        let files = dedupe_first_wins(files);

        let dirs_changed = dirs_differ(&self.dirs, &dirs);

        let mut live_slots: HashSet<u32> = HashSet::with_capacity(files.len());
        let mut files_changed = false;

        for (entry, payload) in files {
            if let Some(index) = self.index_of.get(&entry.rel_path).copied() {
                live_slots.insert(index);
                let slot = usize::try_from(index).expect("u32 fits usize");
                let previous = &self.files[slot];
                let unchanged = previous.size == entry.size
                    && previous.mode == entry.mode
                    && previous.mtime == entry.mtime
                    && !previous.is_tombstone();
                if unchanged {
                    continue;
                }
                self.slots[slot] = Some(payload);
                self.files[slot] = entry;
                files_changed = true;
            } else {
                let index = u32::try_from(self.files.len()).expect("file count fits u32");
                live_slots.insert(index);
                self.index_of.insert(entry.rel_path.clone(), index);
                self.slots.push(Some(payload));
                self.files.push(entry);
                files_changed = true;
            }
        }

        let vanished: Vec<u32> = self
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
            self.files[slot] = FileEntry::tombstone();
            self.slots[slot] = None;
            files_changed = true;
        }

        if !dirs_changed && !files_changed {
            return false;
        }

        self.dirs = dirs;
        self.encoded = MountManifest {
            dirs: self.dirs.clone(),
            files: self.files.clone(),
        }
        .encode();
        self.version += 1;
        true
    }

    pub fn encoded(&self) -> &[u8] {
        &self.encoded
    }

    /// The version these bytes were published as.
    pub const fn version(&self) -> u64 {
        self.version
    }

    /// `None` for a tombstone or out-of-range index.
    pub fn slot(&self, index: u32) -> Option<&T> {
        let slot = usize::try_from(index).ok()?;
        self.slots.get(slot)?.as_ref()
    }

    /// Live (non-tombstone) file count and total bytes.
    pub fn live_counts(&self) -> (u32, u64) {
        let mut count = 0u32;
        let mut bytes = 0u64;
        for file in &self.files {
            if file.is_tombstone() {
                continue;
            }
            count = count.saturating_add(1);
            bytes = bytes.saturating_add(file.size);
        }
        (count, bytes)
    }

    /// Restore a previous snapshot (used when a rescan encodes over the cap).
    pub fn restore(&mut self, previous: LiveStateSnapshot<T>) {
        self.dirs = previous.dirs;
        self.files = previous.files;
        self.slots = previous.slots;
        self.index_of = previous.index_of;
        self.encoded = previous.encoded;
        self.version = previous.version;
    }

    pub fn snapshot(&self) -> LiveStateSnapshot<T>
    where
        T: Clone,
    {
        LiveStateSnapshot {
            dirs: self.dirs.clone(),
            files: self.files.clone(),
            slots: self.slots.clone(),
            index_of: self.index_of.clone(),
            encoded: self.encoded.clone(),
            version: self.version,
        }
    }
}

/// Owned copy of [`LiveState`] fields for rollback after an oversize apply.
pub(crate) struct LiveStateSnapshot<T> {
    dirs: Vec<DirEntry>,
    files: Vec<FileEntry>,
    slots: Vec<Option<T>>,
    index_of: HashMap<String, u32>,
    encoded: Vec<u8>,
    version: u64,
}

fn dedupe_first_wins<T>(files: Vec<(FileEntry, T)>) -> Vec<(FileEntry, T)> {
    let mut seen = HashSet::with_capacity(files.len());
    let mut out = Vec::with_capacity(files.len());
    for (entry, payload) in files {
        if !seen.insert(entry.rel_path.clone()) {
            continue;
        }
        out.push((entry, payload));
    }
    out
}

fn dirs_differ(previous: &[DirEntry], next: &[DirEntry]) -> bool {
    if previous.len() != next.len() {
        return true;
    }
    let prev: HashMap<&str, (u32, i64)> = previous
        .iter()
        .map(|dir| (dir.rel_path.as_str(), (dir.mode, dir.mtime)))
        .collect();
    for dir in next {
        if prev.get(dir.rel_path.as_str()) != Some(&(dir.mode, dir.mtime)) {
            return true;
        }
    }
    // Same keys and values; length match already implies no extras in `previous`.
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    // wasm32 harness — see `transport_mode.rs` for why.
    use wasm_bindgen_test::wasm_bindgen_test as test;

    fn entry(path: &str, size: u64) -> FileEntry {
        FileEntry {
            rel_path: path.to_owned(),
            size,
            mode: 0o644,
            mtime: 1,
        }
    }

    fn state(files: &[(&str, u64)]) -> LiveState<()> {
        LiveState::new(
            Vec::new(),
            files
                .iter()
                .map(|(path, size)| (entry(path, *size), ()))
                .collect(),
        )
    }

    fn apply(state: &mut LiveState<()>, files: &[(&str, u64)]) -> bool {
        state.apply(
            Vec::new(),
            files
                .iter()
                .map(|(path, size)| (entry(path, *size), ()))
                .collect(),
        )
    }

    #[test]
    fn an_unchanged_tree_produces_no_change() {
        let mut tree = state(&[("a", 1), ("b", 2)]);
        assert!(!apply(&mut tree, &[("a", 1), ("b", 2)]));
    }

    #[test]
    fn a_removed_file_leaves_its_slot_alone() {
        let mut tree = state(&[("a", 1), ("b", 2), ("c", 3)]);
        assert!(apply(&mut tree, &[("a", 1), ("c", 3)]));
        assert!(tree.slot(1).is_none(), "a tombstone serves nothing");
        assert!(tree.slot(2).is_some());
        let manifest = MountManifest::decode(tree.encoded()).expect("decode");
        assert!(manifest.files[1].is_tombstone());
        assert_eq!(manifest.files[2].rel_path, "c");
    }

    #[test]
    fn a_new_file_appends_rather_than_sorting_in() {
        let mut tree = state(&[("m", 1)]);
        assert!(apply(&mut tree, &[("aaa", 9), ("m", 1)]));
        let manifest = MountManifest::decode(tree.encoded()).expect("decode");
        assert_eq!(manifest.files[0].rel_path, "m");
        assert_eq!(manifest.files[1].rel_path, "aaa");
        assert_eq!(manifest.files[1].size, 9);
    }

    #[test]
    fn a_recreated_file_reclaims_its_original_index() {
        let mut tree = state(&[("a", 1), ("b", 2)]);
        assert!(apply(&mut tree, &[("a", 1)]));
        assert!(tree.slot(1).is_none());
        assert!(apply(&mut tree, &[("a", 1), ("b", 7)]));
        assert!(tree.slot(1).is_some());
        let manifest = MountManifest::decode(tree.encoded()).expect("decode");
        assert_eq!(manifest.files[1].rel_path, "b");
        assert_eq!(manifest.files[1].size, 7);
    }

    #[test]
    fn a_grown_file_reports_its_new_size() {
        let mut tree = state(&[("a", 1)]);
        assert!(apply(&mut tree, &[("a", 4096)]));
        let manifest = MountManifest::decode(tree.encoded()).expect("decode");
        assert_eq!(manifest.files[0].size, 4096);
    }

    #[test]
    fn the_served_manifest_keeps_tombstones_in_place() {
        let mut tree = state(&[("a", 1), ("b", 2), ("c", 3)]);
        assert!(apply(&mut tree, &[("a", 1), ("c", 3)]));
        let manifest = MountManifest::decode(tree.encoded()).expect("decode");
        assert_eq!(manifest.files.len(), 3, "the slot survives the file");
        assert!(manifest.files[1].is_tombstone());
        assert_eq!(manifest.files[2].rel_path, "c");
    }

    #[test]
    fn duplicate_rel_paths_are_first_wins() {
        let mut tree = state(&[]);
        assert!(tree.apply(
            Vec::new(),
            vec![
                (entry("a", 1), ()),
                (entry("a", 99), ()),
                (entry("b", 2), ()),
            ],
        ));
        let manifest = MountManifest::decode(tree.encoded()).expect("decode");
        assert_eq!(manifest.files.len(), 2);
        assert_eq!(manifest.files[0].size, 1, "first listing wins");
        assert_eq!(manifest.files[1].rel_path, "b");
    }

    #[test]
    fn live_counts_skip_tombstones() {
        let mut tree = state(&[("a", 10), ("b", 20), ("c", 30)]);
        assert_eq!(tree.live_counts(), (3, 60));
        assert!(apply(&mut tree, &[("a", 10), ("c", 30)]));
        assert_eq!(tree.live_counts(), (2, 40));
    }

    #[test]
    fn restore_rolls_back_an_apply() {
        let mut tree = state(&[("a", 1)]);
        let snap = tree.snapshot();
        assert!(apply(&mut tree, &[("a", 1), ("b", 2)]));
        assert_eq!(tree.live_counts(), (2, 3));
        tree.restore(snap);
        assert_eq!(tree.live_counts(), (1, 1));
        assert!(tree.slot(1).is_none());
    }
}
