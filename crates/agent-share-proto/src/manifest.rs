//! The mount manifest: the complete tree listing a consumer turns into a
//! filesystem, plus the status byte leading every response.

use anyhow::{Context, Result, bail};
use serde::Serialize;

/// One directory in the shared tree (every directory, not just empty ones —
/// the consumer builds its tree directly from this list).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DirEntry {
    /// `/`-separated path relative to the shared root.
    pub rel_path: String,
    pub mode: u32,
    /// Seconds since the epoch; 0 when unknown.
    pub mtime: i64,
}

/// One file in the shared tree. Its position in [`MountManifest::files`] is
/// the index READ requests address it by — no hash: bytes are fetched lazily,
/// so hashing the tree up-front would defeat the point.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileEntry {
    /// `/`-separated path relative to the shared root.
    pub rel_path: String,
    pub size: u64,
    pub mode: u32,
    /// Seconds since the epoch; 0 when unknown.
    pub mtime: i64,
}

impl FileEntry {
    /// Whether this slot is a tombstone: reserved, but holding no file.
    ///
    /// A file's position is its READ address, so removing one cannot compact
    /// the list — every index after it would shift, and a consumer still
    /// holding an old index would silently read a different file. Removal
    /// leaves the slot behind instead. An empty `rel_path` is the marker,
    /// which no live entry can collide with: every real path is built from a
    /// directory entry's name and is therefore non-empty.
    #[must_use]
    pub fn is_tombstone(&self) -> bool {
        self.rel_path.is_empty()
    }

    /// The placeholder left in place of a file that is gone.
    #[must_use]
    pub fn tombstone() -> Self {
        Self {
            rel_path: String::new(),
            size: 0,
            mode: 0,
            mtime: 0,
        }
    }
}

/// The mount manifest: the complete tree listing a consumer turns into a
/// filesystem. Distinct from the file-transfer manifest — mount needs
/// mode/mtime and explicit dirs, and deliberately carries no content hashes.
///
/// `files` may contain tombstones once the producer has been watching a tree
/// that changed; see [`FileEntry::is_tombstone`]. Consumers skip them rather
/// than treating them as malformed, since the position must survive.
///
/// Wire layout (little-endian):
/// `dir_count(u32) [path_len(u16) ‖ path ‖ mode(u32) ‖ mtime(i64)]…`
/// `file_count(u32) [path_len(u16) ‖ path ‖ size(u64) ‖ mode(u32) ‖ mtime(i64)]…`
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct MountManifest {
    pub dirs: Vec<DirEntry>,
    pub files: Vec<FileEntry>,
}

/// Domain separator, distinct from every other label in the tree so this
/// derivation can never collide with one of the mesh engine's own. Mirrors
/// `mesh_key::SHARE_MESH_LABEL`.
const TREE_LABEL: &[u8] = b"agent-share/tree/v1";

/// Hex characters kept from the digest.
///
/// Eight bytes of SHA-256. This is an *agreement* check between peers who each
/// already hold the bytes, not a security boundary: a peer that wants to lie
/// about its tree can simply publish someone else's fingerprint, and mesh
/// membership already implies the full read capability. What it has to survive
/// is accidental collision across the trees one share sees in its lifetime,
/// and 64 bits is far more than that needs. The cap matters because the card
/// rides a CRDT under a 3840-byte frame ceiling.
const TREE_FINGERPRINT_HEX: usize = 16;

/// Fingerprint the **exact bytes** `OP_MANIFEST` returned.
///
/// Takes bytes rather than a [`MountManifest`] on purpose. The producer holds
/// the wire bytes already, and so does a consumer at the moment it reads them;
/// hashing those directly means the two agree without either of them having to
/// re-encode. [`MountManifest::fingerprint`] is the convenience for callers
/// that kept only the struct, and it is safe because `decode` → `encode`
/// round-trips byte-for-byte — pinned by `encoding_is_canonical` below, which
/// is what stops two peers on one tree from computing different fingerprints.
#[must_use]
pub fn manifest_fingerprint(manifest_bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(TREE_LABEL);
    hasher.update(manifest_bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(TREE_FINGERPRINT_HEX);
    for byte in digest.iter().take(TREE_FINGERPRINT_HEX / 2) {
        use std::fmt::Write as _;
        // `expect`-free: writing to a String cannot fail.
        let _ = write!(out, "{byte:02x}");
    }
    out
}

impl MountManifest {
    /// This tree's fingerprint, as published on [`crate::PeerCard`]'s `tree`.
    ///
    /// Equal to [`manifest_fingerprint`] over the bytes this manifest was
    /// decoded from. Prefer the free function when the wire bytes are still in
    /// hand: it cannot drift, whereas this one leans on the encoding being
    /// canonical.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        manifest_fingerprint(&self.encode())
    }

    /// # Panics
    /// If the tree holds more than `u32::MAX` directories or files, or a path
    /// longer than `u16::MAX` bytes. `scan` bounds both well below these, so
    /// only a hand-built manifest can trip it.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(
            &u32::try_from(self.dirs.len())
                .expect("dir count fits u32")
                .to_le_bytes(),
        );
        for dir in &self.dirs {
            encode_path(&mut out, &dir.rel_path);
            out.extend_from_slice(&dir.mode.to_le_bytes());
            out.extend_from_slice(&dir.mtime.to_le_bytes());
        }
        out.extend_from_slice(
            &u32::try_from(self.files.len())
                .expect("file count fits u32")
                .to_le_bytes(),
        );
        for file in &self.files {
            encode_path(&mut out, &file.rel_path);
            out.extend_from_slice(&file.size.to_le_bytes());
            out.extend_from_slice(&file.mode.to_le_bytes());
            out.extend_from_slice(&file.mtime.to_le_bytes());
        }
        out
    }

    /// Fold a watch delta into this manifest.
    ///
    /// Every consumer needs exactly this, so it lives here rather than once in
    /// the mount client and again in the browser's, where the two could drift
    /// into disagreeing about what a share contains.
    ///
    /// Upserts are placed at the index the producer assigned, growing the list
    /// with tombstones if a frame was somehow skipped — the position is the
    /// READ address, so it is placed, never appended-wherever.
    ///
    /// # Panics
    /// On a target where a `u32` index does not fit a `usize`, which is none
    /// this is built for.
    pub fn apply(&mut self, delta: &ManifestDelta) {
        if !delta.dirs_removed.is_empty() {
            let removed: std::collections::HashSet<&str> =
                delta.dirs_removed.iter().map(String::as_str).collect();
            self.dirs
                .retain(|dir| !removed.contains(dir.rel_path.as_str()));
        }
        if !delta.dirs_upserted.is_empty() {
            // Decide first, mutate second: the lookup index borrows `dirs`,
            // and rewriting entries under it would invalidate the positions
            // it holds.
            let (updates, appended) = {
                let at: std::collections::HashMap<&str, usize> = self
                    .dirs
                    .iter()
                    .enumerate()
                    .map(|(position, dir)| (dir.rel_path.as_str(), position))
                    .collect();
                let mut updates: Vec<(usize, DirEntry)> = Vec::new();
                let mut appended: Vec<DirEntry> = Vec::new();
                for dir in &delta.dirs_upserted {
                    match at.get(dir.rel_path.as_str()) {
                        Some(&position) => updates.push((position, dir.clone())),
                        None => appended.push(dir.clone()),
                    }
                }
                (updates, appended)
            };
            for (position, dir) in updates {
                self.dirs[position] = dir;
            }
            self.dirs.extend(appended);
        }
        for (index, file) in &delta.files_upserted {
            let slot = usize::try_from(*index).expect("u32 fits usize");
            if slot >= self.files.len() {
                self.files.resize(slot + 1, FileEntry::tombstone());
            }
            self.files[slot] = file.clone();
        }
        for index in &delta.files_removed {
            let slot = usize::try_from(*index).expect("u32 fits usize");
            if let Some(entry) = self.files.get_mut(slot) {
                *entry = FileEntry::tombstone();
            }
        }
    }

    /// Decode a manifest received from the producer. Incremental: every read
    /// is bounds-checked against the remaining bytes, so a hostile count can
    /// cause a decode error but never an unbounded allocation.
    ///
    /// # Errors
    /// Truncated input, a non-UTF-8 path, or trailing garbage.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut cursor = Cursor { bytes, pos: 0 };
        let dir_count = cursor.take_u32()?;
        let mut dirs = Vec::new();
        for _ in 0..dir_count {
            let rel_path = cursor.take_path()?;
            let mode = cursor.take_u32()?;
            let mtime = cursor.take_i64()?;
            dirs.push(DirEntry {
                rel_path,
                mode,
                mtime,
            });
        }
        let file_count = cursor.take_u32()?;
        let mut files = Vec::new();
        for _ in 0..file_count {
            let rel_path = cursor.take_path()?;
            let size = cursor.take_u64()?;
            let mode = cursor.take_u32()?;
            let mtime = cursor.take_i64()?;
            files.push(FileEntry {
                rel_path,
                size,
                mode,
                mtime,
            });
        }
        if cursor.pos != bytes.len() {
            bail!("trailing bytes after the manifest");
        }
        Ok(Self { dirs, files })
    }
}

fn encode_path(out: &mut Vec<u8>, path: &str) {
    let len = u16::try_from(path.len()).expect("scan rejects paths over MAX_REL_PATH");
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(path.as_bytes());
}

/// What changed in the shared tree since the last frame on a watch stream.
///
/// Deltas rather than whole manifests because the tree can be enormous: a
/// 74k-file share encodes to several MB, and re-sending that on every `touch`
/// would cost more than the file bytes ever do.
///
/// # The index invariant
///
/// A file's index is its READ address, so an index that changed meaning
/// between two frames would silently serve the wrong file's bytes to any
/// consumer still holding the old number — a corrupt download, not an error.
/// The producer therefore assigns indices **append-only** for the life of a
/// `serve`: [`files_upserted`](Self::files_upserted) either updates a slot
/// in place (same path, new size/mtime) or appends a fresh one, and
/// [`files_removed`](Self::files_removed) tombstones a slot that is never
/// reused. Applying a delta out of order, or skipping one, breaks this — which
/// is why watch frames ride a single ordered QUIC stream and carry no
/// generation number to resynchronise against.
///
/// Directories are keyed by path instead, since nothing addresses them by
/// position.
///
/// Wire layout (little-endian):
/// `dirs_upserted(u32) [path_len(u16) ‖ path ‖ mode(u32) ‖ mtime(i64)]…`
/// `dirs_removed(u32) [path_len(u16) ‖ path]…`
/// `files_upserted(u32) [index(u32) ‖ path_len(u16) ‖ path ‖ size(u64) ‖ mode(u32) ‖ mtime(i64)]…`
/// `files_removed(u32) [index(u32)]…`
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct ManifestDelta {
    /// Directories added, or whose mode/mtime changed.
    pub dirs_upserted: Vec<DirEntry>,
    /// Directories that are gone, by path.
    pub dirs_removed: Vec<String>,
    /// Files added or changed, each with the index it occupies.
    pub files_upserted: Vec<(u32, FileEntry)>,
    /// Indices whose file is gone. The slot stays allocated forever.
    pub files_removed: Vec<u32>,
}

impl ManifestDelta {
    /// Whether this delta would change anything, so the producer can skip
    /// waking every watcher for a rescan that found nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.dirs_upserted.is_empty()
            && self.dirs_removed.is_empty()
            && self.files_upserted.is_empty()
            && self.files_removed.is_empty()
    }

    /// # Panics
    /// If a list is longer than `u32::MAX`, or a path longer than
    /// `u16::MAX` bytes — the same bounds [`MountManifest::encode`] assumes.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(
            &u32::try_from(self.dirs_upserted.len())
                .expect("dir count fits u32")
                .to_le_bytes(),
        );
        for dir in &self.dirs_upserted {
            encode_path(&mut out, &dir.rel_path);
            out.extend_from_slice(&dir.mode.to_le_bytes());
            out.extend_from_slice(&dir.mtime.to_le_bytes());
        }
        out.extend_from_slice(
            &u32::try_from(self.dirs_removed.len())
                .expect("dir count fits u32")
                .to_le_bytes(),
        );
        for path in &self.dirs_removed {
            encode_path(&mut out, path);
        }
        out.extend_from_slice(
            &u32::try_from(self.files_upserted.len())
                .expect("file count fits u32")
                .to_le_bytes(),
        );
        for (index, file) in &self.files_upserted {
            out.extend_from_slice(&index.to_le_bytes());
            encode_path(&mut out, &file.rel_path);
            out.extend_from_slice(&file.size.to_le_bytes());
            out.extend_from_slice(&file.mode.to_le_bytes());
            out.extend_from_slice(&file.mtime.to_le_bytes());
        }
        out.extend_from_slice(
            &u32::try_from(self.files_removed.len())
                .expect("file count fits u32")
                .to_le_bytes(),
        );
        for index in &self.files_removed {
            out.extend_from_slice(&index.to_le_bytes());
        }
        out
    }

    /// Decode a delta received from the producer. Bounds-checked the same way
    /// [`MountManifest::decode`] is, for the same reason: the counts are
    /// attacker-controlled.
    ///
    /// # Errors
    /// Truncated input, a non-UTF-8 path, or trailing garbage.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut cursor = Cursor { bytes, pos: 0 };
        let mut dirs_upserted = Vec::new();
        for _ in 0..cursor.take_u32()? {
            let rel_path = cursor.take_path()?;
            let mode = cursor.take_u32()?;
            let mtime = cursor.take_i64()?;
            dirs_upserted.push(DirEntry {
                rel_path,
                mode,
                mtime,
            });
        }
        let mut dirs_removed = Vec::new();
        for _ in 0..cursor.take_u32()? {
            dirs_removed.push(cursor.take_path()?);
        }
        let mut files_upserted = Vec::new();
        for _ in 0..cursor.take_u32()? {
            let index = cursor.take_u32()?;
            let rel_path = cursor.take_path()?;
            let size = cursor.take_u64()?;
            let mode = cursor.take_u32()?;
            let mtime = cursor.take_i64()?;
            files_upserted.push((
                index,
                FileEntry {
                    rel_path,
                    size,
                    mode,
                    mtime,
                },
            ));
        }
        let mut files_removed = Vec::new();
        for _ in 0..cursor.take_u32()? {
            files_removed.push(cursor.take_u32()?);
        }
        if cursor.pos != bytes.len() {
            bail!("trailing bytes after the delta");
        }
        Ok(Self {
            dirs_upserted,
            dirs_removed,
            files_upserted,
            files_removed,
        })
    }
}

/// The result byte leading every READ response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadStatus {
    Ok,
    BadIndex,
    Io,
    LenOverCap,
}

impl ReadStatus {
    #[must_use]
    pub fn to_byte(self) -> u8 {
        match self {
            ReadStatus::Ok => 0,
            ReadStatus::BadIndex => 1,
            ReadStatus::Io => 2,
            ReadStatus::LenOverCap => 3,
        }
    }

    /// # Errors
    /// The byte is not a known status.
    pub fn from_byte(byte: u8) -> Result<Self> {
        match byte {
            0 => Ok(ReadStatus::Ok),
            1 => Ok(ReadStatus::BadIndex),
            2 => Ok(ReadStatus::Io),
            3 => Ok(ReadStatus::LenOverCap),
            other => bail!("unknown read status: {other}"),
        }
    }
}

/// A bounds-checked reader over the received manifest bytes.
struct Cursor<'bytes> {
    bytes: &'bytes [u8],
    pos: usize,
}

impl Cursor<'_> {
    fn take(&mut self, len: usize) -> Result<&[u8]> {
        let end = self
            .pos
            .checked_add(len)
            .filter(|&end| end <= self.bytes.len())
            .context("truncated manifest")?;
        let slice = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn take_u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(
            self.take(2)?.try_into().expect("2 bytes"),
        ))
    }

    fn take_u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("4 bytes"),
        ))
    }

    fn take_u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().expect("8 bytes"),
        ))
    }

    fn take_i64(&mut self) -> Result<i64> {
        Ok(i64::from_le_bytes(
            self.take(8)?.try_into().expect("8 bytes"),
        ))
    }

    fn take_path(&mut self) -> Result<String> {
        let len = usize::from(self.take_u16()?);
        let raw = self.take(len)?;
        String::from_utf8(raw.to_vec()).context("non-UTF-8 path in manifest")
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DirEntry, FileEntry, ManifestDelta, MountManifest, ReadStatus, manifest_fingerprint,
    };

    fn sample_delta() -> ManifestDelta {
        ManifestDelta {
            dirs_upserted: vec![DirEntry {
                rel_path: "docs/new".to_owned(),
                mode: 0o755,
                mtime: 1_700_000_002,
            }],
            dirs_removed: vec!["docs/empty".to_owned()],
            files_upserted: vec![
                (
                    0,
                    FileEntry {
                        rel_path: "README.md".to_owned(),
                        size: 99,
                        mode: 0o644,
                        mtime: 1_700_000_003,
                    },
                ),
                (
                    2,
                    FileEntry {
                        rel_path: "docs/new/added.md".to_owned(),
                        size: 7,
                        mode: 0o600,
                        mtime: -1,
                    },
                ),
            ],
            files_removed: vec![1, u32::MAX],
        }
    }

    #[test]
    fn applying_a_delta_keeps_indices_put() {
        let mut manifest = sample();
        // Drop file 0, change file 1, add a new one at slot 2.
        manifest.apply(&ManifestDelta {
            dirs_upserted: Vec::new(),
            dirs_removed: vec!["docs/empty".to_owned()],
            files_upserted: vec![
                (
                    1,
                    FileEntry {
                        rel_path: "docs/guide.md".to_owned(),
                        size: 500,
                        mode: 0o600,
                        mtime: 9,
                    },
                ),
                (
                    2,
                    FileEntry {
                        rel_path: "new.txt".to_owned(),
                        size: 3,
                        mode: 0o644,
                        mtime: 9,
                    },
                ),
            ],
            files_removed: vec![0],
        });
        assert!(
            manifest.files[0].is_tombstone(),
            "the removed slot survives"
        );
        assert_eq!(manifest.files[1].size, 500);
        assert_eq!(manifest.files[2].rel_path, "new.txt");
        assert_eq!(manifest.dirs.len(), 1, "docs/empty is gone");
        assert_eq!(manifest.dirs[0].rel_path, "docs");
    }

    #[test]
    fn an_upsert_past_the_end_pads_with_tombstones() {
        let mut manifest = sample();
        manifest.apply(&ManifestDelta {
            files_upserted: vec![(
                5,
                FileEntry {
                    rel_path: "far.txt".to_owned(),
                    size: 1,
                    mode: 0o644,
                    mtime: 0,
                },
            )],
            ..ManifestDelta::default()
        });
        assert_eq!(manifest.files.len(), 6);
        assert!(manifest.files[3].is_tombstone());
        assert_eq!(manifest.files[5].rel_path, "far.txt");
    }

    #[test]
    fn delta_round_trips() {
        let delta = sample_delta();
        assert_eq!(
            ManifestDelta::decode(&delta.encode()).expect("decode"),
            delta
        );
    }

    #[test]
    fn empty_delta_round_trips_and_reports_empty() {
        let delta = ManifestDelta::default();
        assert!(delta.is_empty());
        assert_eq!(
            ManifestDelta::decode(&delta.encode()).expect("decode"),
            delta
        );
        assert!(!sample_delta().is_empty());
    }

    #[test]
    fn truncated_delta_is_rejected() {
        let encoded = sample_delta().encode();
        for len in 0..encoded.len() {
            assert!(
                ManifestDelta::decode(&encoded[..len]).is_err(),
                "prefix of {len} bytes must not decode"
            );
        }
    }

    #[test]
    fn delta_with_trailing_bytes_is_rejected() {
        let mut encoded = sample_delta().encode();
        encoded.push(0);
        assert!(ManifestDelta::decode(&encoded).is_err());
    }

    fn sample() -> MountManifest {
        MountManifest {
            dirs: vec![
                DirEntry {
                    rel_path: "docs".to_owned(),
                    mode: 0o755,
                    mtime: 1_700_000_000,
                },
                DirEntry {
                    rel_path: "docs/empty".to_owned(),
                    mode: 0o700,
                    mtime: 0,
                },
            ],
            files: vec![
                FileEntry {
                    rel_path: "README.md".to_owned(),
                    size: 42,
                    mode: 0o644,
                    mtime: 1_700_000_001,
                },
                FileEntry {
                    rel_path: "docs/guide.md".to_owned(),
                    size: 0,
                    mode: 0o600,
                    mtime: -1,
                },
            ],
        }
    }

    #[test]
    fn manifest_round_trips() {
        let manifest = sample();
        let decoded = MountManifest::decode(&manifest.encode()).expect("decode");
        assert_eq!(decoded, manifest);
    }

    /// **What makes [`MountManifest::fingerprint`] safe.**
    ///
    /// The fingerprint is defined over the exact bytes `OP_MANIFEST` returned.
    /// A producer has those bytes; a consumer decoded them and threw them away,
    /// so its convenience path re-encodes. If the encoding were not canonical
    /// the two would disagree, and two peers on the *same* tree would read as
    /// being on different ones — rejecting a perfectly good source.
    #[test]
    fn encoding_is_canonical() {
        let bytes = sample().encode();
        let reencoded = MountManifest::decode(&bytes).expect("decode").encode();
        assert_eq!(bytes, reencoded, "decode then encode must be byte-exact");
    }

    #[test]
    fn a_fingerprint_is_short_stable_hex() {
        let manifest = sample();
        let print = manifest.fingerprint();
        assert_eq!(print, manifest.fingerprint(), "not stable");
        assert_eq!(
            print.len(),
            16,
            "16 hex chars keeps the card inside a frame"
        );
        assert!(print.chars().all(|ch| ch.is_ascii_hexdigit()));
    }

    /// The producer fingerprints wire bytes, the consumer fingerprints a
    /// re-encode. They have to land on the same string or the guard is useless.
    #[test]
    fn both_paths_to_a_fingerprint_agree() {
        let bytes = sample().encode();
        assert_eq!(
            manifest_fingerprint(&bytes),
            MountManifest::decode(&bytes).expect("decode").fingerprint()
        );
    }

    /// The whole point: a tree that changed must fingerprint differently, or a
    /// peer on a stale manifest passes as current.
    #[test]
    fn a_changed_tree_changes_its_fingerprint() {
        let base = sample();
        let mut grown = base.clone();
        grown.files[0].size += 1;
        assert_ne!(base.fingerprint(), grown.fingerprint(), "a resized file");

        let mut renamed = base.clone();
        renamed.files[0].rel_path = "renamed.txt".to_owned();
        assert_ne!(base.fingerprint(), renamed.fingerprint(), "a renamed file");

        let mut reordered = base.clone();
        reordered.files.swap(0, 1);
        assert_ne!(
            base.fingerprint(),
            reordered.fingerprint(),
            "order is the READ address, so a reorder is a different tree"
        );
    }

    /// Domain separation: the fingerprint must not be a bare SHA-256 of the
    /// bytes, or it could collide with some other derivation over the same
    /// input.
    #[test]
    fn the_fingerprint_is_domain_separated() {
        use sha2::{Digest, Sha256};
        let bytes = sample().encode();
        let bare = Sha256::digest(&bytes);
        let mut bare_hex = String::new();
        for byte in bare.iter().take(8) {
            use std::fmt::Write as _;
            let _ = write!(bare_hex, "{byte:02x}");
        }
        assert_ne!(manifest_fingerprint(&bytes), bare_hex);
    }

    #[test]
    fn truncated_manifest_is_rejected() {
        let encoded = sample().encode();
        for len in 0..encoded.len() {
            assert!(
                MountManifest::decode(&encoded[..len]).is_err(),
                "prefix of {len} bytes must not decode"
            );
        }
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        let mut encoded = sample().encode();
        encoded.push(0);
        assert!(MountManifest::decode(&encoded).is_err());
    }

    #[test]
    fn hostile_count_fails_without_allocating() {
        // Claims u32::MAX dirs but carries none: must error, not OOM.
        let bytes = u32::MAX.to_le_bytes();
        assert!(MountManifest::decode(&bytes).is_err());
    }

    #[test]
    fn read_status_round_trips() {
        for status in [
            ReadStatus::Ok,
            ReadStatus::BadIndex,
            ReadStatus::Io,
            ReadStatus::LenOverCap,
        ] {
            assert_eq!(
                ReadStatus::from_byte(status.to_byte()).expect("round trip"),
                status
            );
        }
        assert!(ReadStatus::from_byte(9).is_err());
    }
}
