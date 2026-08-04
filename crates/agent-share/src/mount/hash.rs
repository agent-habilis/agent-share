//! Hashing a shared file, on demand and never before.
//!
//! The producer answers [`OP_HASH`] with a file's BLAKE3 root and bao outboard,
//! which is what lets a consumer take the *bytes* from some other peer and still
//! know it got the right ones.
//!
//! **Lazy is the whole point, so it is enforced here rather than assumed.**
//! `manifest.rs` refuses to carry hashes because filling that field would mean
//! hashing at scan time, turning `serve` on a 500 GB tree from a `stat` walk
//! into a full read of it. This module is where that rule could quietly be
//! broken — by hashing on startup, or by warming a cache — so it does neither. A
//! file is read and hashed the first time somebody asks for its root, and the
//! answer is kept so the second asker pays nothing.
//!
//! Everything about *how* bytes are verified lives in `fofoca-blobs`; this is
//! only the part that knows what a share is.

use std::path::Path;

use anyhow::{Context, Result};
use fofoca_blobs::{BlobStore, FileId, FsStore, Root};

use super::live::LiveTree;

/// The producer's hash cache: outboards for files somebody has asked about.
///
/// Wraps an [`FsStore`], which keeps sidecar metadata beside files it never
/// copies — so a share of a terabyte costs a directory of small files, not a
/// second terabyte.
/// `pub` because it is a parameter of `serve_established`, which
/// `crate::test_support` re-exports for the integration tests. That export is
/// `#[doc(hidden)]`, so this is reachable in the type system and invisible in
/// the docs — the same trade `LiveTree` beside it makes.
#[derive(Debug)]
pub struct HashCache {
    store: FsStore,
}

impl HashCache {
    /// Open a cache under `dir`.
    ///
    /// # Errors
    /// The directory cannot be created.
    pub fn open(dir: &Path) -> Result<Self> {
        Ok(Self {
            store: FsStore::open(dir).context("opening the hash cache")?,
        })
    }

    /// The root and outboard for manifest index `index`, hashing the file if
    /// this is the first time anyone has asked.
    ///
    /// Returns `None` for an index that is out of range or tombstoned, or for a
    /// file that has changed since it was last hashed and cannot be re-read.
    /// A caller answers `BadIndex` to all of those: from the far side they are
    /// the same thing, *this peer cannot vouch for that content*.
    pub async fn root_of_index(&self, tree: &LiveTree, index: u32) -> Option<(Root, Vec<u8>)> {
        let path = tree.path_of(index)?;
        let meta = tokio::fs::metadata(&path).await.ok()?;
        let file = FileId {
            key: path.to_string_lossy().into_owned(),
            size: meta.len(),
            mtime: meta
                .modified()
                .ok()
                .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |since| since.as_secs().cast_signed()),
        };

        // Already hashed, and the file has not moved underneath it. The version
        // gate lives in the store, so a file edited since it was hashed reads
        // as unbound here and is re-hashed below rather than answered stale.
        if let Ok(Some(root)) = self.store.bind(&file).await
            && let Ok(Some(outboard)) = self.store.outboard(root).await
        {
            return Some((root, outboard));
        }

        // First ask for this file. Read it once, hash it, keep the outboard.
        //
        // Whole-file read, deliberately: bao needs every byte to build a tree,
        // and this is the one moment a share pays for a file it is serving. It
        // happens once per file per version, only for files somebody wants from
        // a third party, and never at startup.
        let bytes = tokio::fs::read(&path).await.ok()?;
        // Re-check: the file may have changed between the stat above and this
        // read, and binding the new bytes under the old size would make every
        // later read fail verification for no visible reason.
        if bytes.len() as u64 != file.size {
            return None;
        }
        let root = self.store.insert_complete(&file, &bytes).await.ok()?;
        let outboard = self.store.outboard(root).await.ok()??;
        Some((root, outboard))
    }
}

#[cfg(test)]
mod tests {
    use super::HashCache;
    use crate::mount::live::LiveTree;
    use std::sync::Arc;

    /// A throwaway directory, as the rest of this crate's tests hand-roll one.
    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            use rand::RngCore as _;
            let path = std::env::temp_dir()
                .join(format!("agent-share-hash-{tag}-{}", rand::rng().next_u64()));
            std::fs::create_dir_all(&path).expect("create temp dir");
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn tree_of(root: &std::path::Path) -> Arc<LiveTree> {
        let (manifest, paths) = crate::mount::scan::scan(root).expect("scan");
        Arc::new(LiveTree::new(root.to_path_buf(), manifest, paths))
    }

    #[tokio::test]
    async fn a_root_is_produced_on_demand_and_reused() {
        let data = TempDir::new("data");
        std::fs::write(data.0.join("a.bin"), vec![7u8; 300_000]).expect("write");
        let tree = tree_of(&data.0);

        let cache_dir = TempDir::new("cache");
        let cache = HashCache::open(&cache_dir.0).expect("open");

        let (root, outboard) = cache
            .root_of_index(&tree, 0)
            .await
            .expect("index 0 must hash");
        assert!(!outboard.is_empty(), "300 KB needs a tree");

        // Second ask is served from the cache. Same answer, and the point of
        // having a cache at all.
        let (again, _) = cache.root_of_index(&tree, 0).await.expect("cached");
        assert_eq!(root, again);
    }

    /// **The laziness rule, as a test rather than a comment.** Opening a cache
    /// must read nothing: hashing at startup is what would turn `serve` on a
    /// large tree from a `stat` walk into a full read of it.
    #[tokio::test]
    async fn opening_a_cache_hashes_nothing() {
        let data = TempDir::new("lazy-data");
        for name in ["a.bin", "b.bin", "c.bin"] {
            std::fs::write(data.0.join(name), vec![1u8; 100_000]).expect("write");
        }
        let _tree = tree_of(&data.0);

        let cache_dir = TempDir::new("lazy-cache");
        let _cache = HashCache::open(&cache_dir.0).expect("open");

        // An outboard would be the only reason for a file to appear here.
        let sidecars = std::fs::read_dir(&cache_dir.0)
            .expect("read cache dir")
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "obao")
            })
            .count();
        assert_eq!(sidecars, 0, "opening a cache must not hash anything");
    }

    #[tokio::test]
    async fn an_index_past_the_tree_has_no_root() {
        let data = TempDir::new("oob-data");
        std::fs::write(data.0.join("a.bin"), b"hi").expect("write");
        let tree = tree_of(&data.0);
        let cache_dir = TempDir::new("oob-cache");
        let cache = HashCache::open(&cache_dir.0).expect("open");

        assert!(cache.root_of_index(&tree, 424_242).await.is_none());
    }

    /// A file edited after it was hashed must be re-hashed, not answered from
    /// the outboard describing content that is gone.
    #[tokio::test]
    async fn an_edited_file_is_rehashed_rather_than_answered_stale() {
        let data = TempDir::new("edit-data");
        let path = data.0.join("a.bin");
        std::fs::write(&path, vec![1u8; 200_000]).expect("write");
        let tree = tree_of(&data.0);

        let cache_dir = TempDir::new("edit-cache");
        let cache = HashCache::open(&cache_dir.0).expect("open");
        let (before, _) = cache.root_of_index(&tree, 0).await.expect("first hash");

        // Different content *and* a different size, so the version gate fires
        // on a filesystem whose mtime resolution is coarse.
        std::fs::write(&path, vec![2u8; 200_001]).expect("rewrite");
        let rescanned = tree_of(&data.0);
        let (after, _) = cache.root_of_index(&rescanned, 0).await.expect("re-hash");

        assert_ne!(
            before, after,
            "an edited file must not keep the root of its previous content"
        );
    }
}
