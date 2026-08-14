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

/// Catch a manifest up to `answer`, and prove the result.
///
/// The consumer's half of `OP_MANIFEST_SINCE`. `held` is the signed manifest
/// this peer already has; `answer` is the chain a producer sent to carry it
/// forward.
///
/// # Why an unsigned delta is safe to apply
///
/// Nothing signs a delta. Nothing needs to. The chain is applied to bytes this
/// peer already trusts, the result is re-encoded, and the **creator's**
/// signature is checked over that reconstruction — so the proof covers the
/// outcome rather than the transport. Encoding is canonical
/// (`encoding_is_canonical`), so any chain that is wrong in any way — truncated,
/// reordered, replayed, or forged by a peer relaying it — reconstructs bytes the
/// signature does not cover, and this fails rather than adopting them. The
/// caller then asks for the whole manifest, which is what it would have done
/// anyway.
///
/// `author` of `None` is an unsigned share, where there is nothing to check and
/// nothing to forge: such a share is only ever as trustworthy as the endpoint it
/// came from, exactly as `OP_MANIFEST` on one is.
///
/// # Errors
/// A delta does not decode, the target version goes backwards, or the
/// reconstruction does not carry the creator's signature.
pub fn apply_since(
    held: &crate::authorship::SignedManifest,
    answer: &crate::framing::ManifestSince,
    author: Option<&crate::authorship::PublicKey>,
) -> Result<crate::authorship::SignedManifest> {
    if answer.target_version < held.version {
        bail!(
            "a producer offered version {} after version {}; refusing to roll back",
            answer.target_version,
            held.version
        );
    }
    let mut manifest = MountManifest::decode(&held.manifest)?;
    for delta in &answer.deltas {
        manifest.apply(&ManifestDelta::decode(delta)?);
    }
    let rebuilt = manifest.encode();
    let signed = crate::authorship::SignedManifest {
        version: answer.target_version,
        signature: answer.signature,
        manifest: rebuilt,
    };
    if let Some(author) = author {
        signed.accept(author, held.version)?;
    }
    Ok(signed)
}

/// Fold one `OP_WATCH` frame into the manifest a consumer holds.
///
/// The single place a watch frame is believed, shared by the native consumer and
/// the browser one so neither can drift into trusting more than the other.
///
/// Both frame kinds carry the creator's signature — a whole envelope, or a
/// [`crate::framing::ManifestSince`] whose deltas rebuild one. So **who relayed
/// the frame does not matter**: only the holder of the authorship key can author
/// a version, and [`crate::authorship::SignedManifest::accept`] refuses one that
/// goes backwards. That is what lets a peer follow a share through a seeder
/// while still being unable to be lied to by it.
///
/// `held` is advanced in place on success and left untouched on failure, so a
/// rejected frame leaves the consumer on the last tree it did believe.
///
/// # Errors
/// The frame does not decode, its kind is unknown, the signature is not the
/// creator's, or the version goes backwards.
pub fn apply_watch_frame(
    held: &mut crate::authorship::SignedManifest,
    kind: u8,
    payload: &[u8],
    author: Option<&crate::authorship::PublicKey>,
) -> Result<MountManifest> {
    let next = match kind {
        crate::framing::WATCH_FRAME_MANIFEST => {
            let offered = crate::authorship::SignedManifest::decode(payload)?;
            if let Some(author) = author {
                offered.accept(author, held.version)?;
            } else if offered.version < held.version {
                bail!(
                    "a peer offered version {} after version {}; refusing to roll back",
                    offered.version,
                    held.version
                );
            }
            offered
        }
        crate::framing::WATCH_FRAME_DELTA => apply_since(
            held,
            &crate::framing::ManifestSince::decode(payload)?,
            author,
        )?,
        other => bail!("unknown watch frame kind: {other}"),
    };
    let manifest = MountManifest::decode(&next.manifest)?;
    *held = next;
    Ok(manifest)
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
        DirEntry, FileEntry, ManifestDelta, MountManifest, ReadStatus, apply_since,
        apply_watch_frame, manifest_fingerprint,
    };
    use crate::authorship::{SIGNATURE_LEN, SignedManifest, sign_manifest};
    use crate::framing::{ManifestSince, WATCH_FRAME_DELTA, WATCH_FRAME_MANIFEST};
    use fofoca_protocol::iroh_base::SecretKey;

    /// The share's creator, and somebody who wishes they were.
    fn creator() -> SecretKey {
        SecretKey::from_bytes(&[7u8; 32])
    }
    fn impostor() -> SecretKey {
        SecretKey::from_bytes(&[9u8; 32])
    }

    fn tree(paths: &[&str]) -> MountManifest {
        MountManifest {
            dirs: Vec::new(),
            files: paths
                .iter()
                .map(|path| FileEntry {
                    rel_path: (*path).to_owned(),
                    size: 1,
                    mode: 0o644,
                    mtime: 0,
                })
                .collect(),
        }
    }

    /// A `WATCH_FRAME_MANIFEST` payload as a producer emits it.
    fn envelope(key: &SecretKey, version: u64, manifest: &MountManifest) -> Vec<u8> {
        let bytes = manifest.encode();
        SignedManifest {
            version,
            signature: sign_manifest(key, version, &bytes),
            manifest: bytes,
        }
        .encode()
    }

    fn held_at(version: u64, manifest: &MountManifest) -> SignedManifest {
        let bytes = manifest.encode();
        SignedManifest {
            version,
            signature: sign_manifest(&creator(), version, &bytes),
            manifest: bytes,
        }
    }

    /// Distribution, not authorship: a frame the creator signed is taken no
    /// matter who relayed it. This is what lets a share outlive its producer.
    #[test]
    fn a_frame_the_creator_signed_is_accepted() {
        let mut held = held_at(1, &tree(&["a"]));
        let next = tree(&["a", "b"]);
        let applied = apply_watch_frame(
            &mut held,
            WATCH_FRAME_MANIFEST,
            &envelope(&creator(), 2, &next),
            Some(&creator().public()),
        )
        .expect("the creator's own frame");
        assert_eq!(applied.files.len(), 2);
        assert_eq!(held.version, 2, "the held version moves with it");
    }

    /// The other half of the invariant: relaying is allowed, authoring is not.
    #[test]
    fn a_frame_signed_by_anyone_else_is_refused() {
        let start = tree(&["a"]);
        let mut held = held_at(1, &start);
        let forged = tree(&["a", "evil"]);
        assert!(
            apply_watch_frame(
                &mut held,
                WATCH_FRAME_MANIFEST,
                &envelope(&impostor(), 2, &forged),
                Some(&creator().public()),
            )
            .is_err(),
            "a seeder must not be able to author a version"
        );
        assert_eq!(
            held.version, 1,
            "a refused frame leaves the held tree alone"
        );
        assert_eq!(held.manifest, start.encode());
    }

    /// A real, creator-signed *older* manifest is still refused — otherwise a
    /// peer could replay one forever and pin everybody to a stale tree.
    #[test]
    fn a_replayed_older_version_is_refused() {
        let mut held = held_at(5, &tree(&["a", "b"]));
        assert!(
            apply_watch_frame(
                &mut held,
                WATCH_FRAME_MANIFEST,
                &envelope(&creator(), 4, &tree(&["a"])),
                Some(&creator().public()),
            )
            .is_err()
        );
        assert_eq!(held.version, 5);
    }

    /// The same manifest arriving twice is ordinary, not an attack.
    #[test]
    fn the_same_version_may_arrive_twice() {
        let same = tree(&["a"]);
        let mut held = held_at(3, &same);
        assert!(
            apply_watch_frame(
                &mut held,
                WATCH_FRAME_MANIFEST,
                &envelope(&creator(), 3, &same),
                Some(&creator().public()),
            )
            .is_ok()
        );
    }

    /// Deltas carry the signature too, so the cheap frame is as safe as the
    /// whole-tree one — the gap the bare-delta format used to leave open.
    #[test]
    fn a_signed_delta_frame_is_verified_against_its_result() {
        let start = tree(&["a"]);
        let mut held = held_at(1, &start);

        let mut rebuilt = start.clone();
        let delta = ManifestDelta {
            dirs_upserted: Vec::new(),
            dirs_removed: Vec::new(),
            files_upserted: vec![(
                1,
                FileEntry {
                    rel_path: "b".to_owned(),
                    size: 1,
                    mode: 0o644,
                    mtime: 0,
                },
            )],
            files_removed: Vec::new(),
        };
        rebuilt.apply(&delta);

        let since = ManifestSince {
            target_version: 2,
            signature: sign_manifest(&creator(), 2, &rebuilt.encode()),
            deltas: vec![delta.encode()],
        };
        let applied = apply_watch_frame(
            &mut held,
            WATCH_FRAME_DELTA,
            &since.encode(),
            Some(&creator().public()),
        )
        .expect("a delta the creator vouched for");
        assert_eq!(applied.files.len(), 2);
        assert_eq!(held.version, 2);
    }

    /// A delta whose signature does not match what it rebuilds is refused —
    /// the reconstruction is checked, not just the arithmetic.
    #[test]
    fn a_delta_that_rebuilds_something_unsigned_is_refused() {
        let mut held = held_at(1, &tree(&["a"]));
        let since = ManifestSince {
            target_version: 2,
            signature: sign_manifest(&creator(), 2, &tree(&["a", "different"]).encode()),
            deltas: vec![
                ManifestDelta {
                    dirs_upserted: Vec::new(),
                    dirs_removed: Vec::new(),
                    files_upserted: vec![(
                        1,
                        FileEntry {
                            rel_path: "b".to_owned(),
                            size: 1,
                            mode: 0o644,
                            mtime: 0,
                        },
                    )],
                    files_removed: Vec::new(),
                }
                .encode(),
            ],
        };
        assert!(
            apply_watch_frame(
                &mut held,
                WATCH_FRAME_DELTA,
                &since.encode(),
                Some(&creator().public()),
            )
            .is_err()
        );
        assert_eq!(held.version, 1);
    }

    /// An unsigned share has no creator to check against, so only the version
    /// rule applies. Stated as a test so the weaker guarantee is deliberate.
    #[test]
    fn an_unsigned_share_still_refuses_a_rollback() {
        let mut held = SignedManifest {
            version: 4,
            signature: [0u8; SIGNATURE_LEN],
            manifest: tree(&["a"]).encode(),
        };
        let older = SignedManifest {
            version: 3,
            signature: [0u8; SIGNATURE_LEN],
            manifest: tree(&["b"]).encode(),
        }
        .encode();
        assert!(apply_watch_frame(&mut held, WATCH_FRAME_MANIFEST, &older, None).is_err());
    }

    #[test]
    fn an_unknown_frame_kind_is_refused() {
        let mut held = held_at(1, &tree(&["a"]));
        assert!(apply_watch_frame(&mut held, 99, b"whatever", Some(&creator().public())).is_err());
    }

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

    /// A caught-up manifest carries the creator's signature, and a tampered
    /// chain does not.
    ///
    /// The whole argument for sending unsigned deltas: the proof covers the
    /// *result*, so a peer relaying the chain cannot change what the consumer
    /// ends up believing. Both halves are asserted here, because only the pair
    /// is the claim — that it works is unremarkable, that it cannot be subverted
    /// is the point.
    #[test]
    fn a_delta_chain_is_only_taken_when_it_rebuilds_what_the_creator_signed() {
        use crate::authorship::{PublicKey, SecretKey, SignedManifest, sign_manifest};
        use crate::framing::ManifestSince;

        let creator = SecretKey::from_bytes(&[9u8; 32]);
        let author: PublicKey = creator.public();

        let held_manifest = sample();
        let held_bytes = held_manifest.encode();
        let held = SignedManifest {
            version: 4,
            signature: sign_manifest(&creator, 4, &held_bytes),
            manifest: held_bytes,
        };

        // What the producer would publish next: one file's size changes.
        let delta = ManifestDelta {
            files_upserted: vec![(
                0,
                FileEntry {
                    rel_path: sample().files[0].rel_path.clone(),
                    size: 4242,
                    mode: 0o644,
                    mtime: 1_700_000_009,
                },
            )],
            ..ManifestDelta::default()
        };
        let mut ahead = sample();
        ahead.apply(&delta);
        let ahead_bytes = ahead.encode();

        let answer = ManifestSince {
            target_version: 5,
            signature: sign_manifest(&creator, 5, &ahead_bytes),
            deltas: vec![delta.encode()],
        };
        let caught_up = apply_since(&held, &answer, Some(&author)).expect("catch up");
        assert_eq!(caught_up.version, 5);
        assert_eq!(
            caught_up.manifest, ahead_bytes,
            "the reconstruction must be byte-exact, or the signature is meaningless"
        );

        // Now the hostile case: a relay swaps in a delta of its own, keeping the
        // creator's signature. The reconstruction stops matching what was
        // signed, so it is refused rather than adopted.
        let tampered = ManifestSince {
            deltas: vec![
                ManifestDelta {
                    files_removed: vec![0],
                    ..ManifestDelta::default()
                }
                .encode(),
            ],
            ..answer.clone()
        };
        assert!(
            apply_since(&held, &tampered, Some(&author)).is_err(),
            "a chain that rebuilds something else must not be believed"
        );

        // And a producer replaying an older version loses to what we hold.
        let backwards = ManifestSince {
            target_version: 3,
            ..answer
        };
        assert!(
            apply_since(&held, &backwards, Some(&author)).is_err(),
            "rolling a consumer backwards must be refused"
        );
    }

    /// The signed envelope is **not** the fingerprint domain.
    ///
    /// `OP_MANIFEST` answers `version ‖ signature ‖ manifest`, and a peer holds
    /// that envelope at exactly the moment it wants to publish `card.tree`.
    /// Fingerprinting what is in hand is the easy mistake, and it is silent: the
    /// tab advertises a well-formed tree string that no other peer computes, so
    /// it vouches for a tree nobody recognises and a consumer that pinned it
    /// rejects the manifest as "changed trees after being vetted". The browser
    /// client did exactly this from the commit that introduced signing until the
    /// one that added this test.
    #[test]
    fn the_envelope_is_not_the_fingerprint_domain() {
        let manifest = sample().encode();
        let envelope = SignedManifest {
            version: 7,
            signature: [0xab; SIGNATURE_LEN],
            manifest: manifest.clone(),
        }
        .encode();
        assert_ne!(
            manifest_fingerprint(&envelope),
            manifest_fingerprint(&manifest),
            "fingerprinting the envelope must not silently pass for the tree"
        );
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
