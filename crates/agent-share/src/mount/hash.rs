//! Addressing a shared file's chunks, on demand and never before.
//!
//! The producer answers `OP_CHUNK_MAP` with a file's ordered leaf row — the
//! `blake3` address of each 64 `KiB` chunk — and `OP_CHUNK` with the bytes at one
//! of those addresses. Together they are what lets a consumer take chunks from
//! *any* peer and still know it got the right ones.
//!
//! **Lazy is the whole point, so it is enforced here rather than assumed.**
//! `manifest.rs` refuses to carry hashes because filling that field would mean
//! hashing at scan time, turning `serve` on a 500 GB tree from a `stat` walk
//! into a full read of it. This module is where that rule could quietly be
//! broken — by hashing on startup, or by warming a cache — so it does neither. A
//! file is read and addressed the first time somebody asks about it, and the
//! answer is kept so the second asker pays nothing.
//!
//! # Nothing is copied
//!
//! The cache wraps a [`FsOrigin`], which records where each address lives and
//! reads through to the user's own file. A share of a terabyte costs a table of
//! 32-byte addresses, not a second terabyte — and that table is ~0.05 % of the
//! content, half what the bao outboard it replaces cost.
//!
//! # Restarts re-address, deliberately
//!
//! The table is in memory only. A restart re-reads a file the first time
//! somebody asks about it again, which is exactly the cost of the first run and
//! is paid per file rather than per tree. Persisting it would be an
//! optimisation with a correctness hazard attached — a stale table describing
//! content that has since moved — and the version gate that would have to guard
//! it is the same one that already re-checks on every read.

use anyhow::Result;
use fofoca_chunks::{ChunkHash, ChunkMap, ChunkSource as _, Coverage, FileId, FsOrigin, Root};

use super::live::LiveTree;

/// The producer's chunk table: leaf rows for files somebody has asked about.
///
/// `pub` because it is a parameter of `serve_established`, which
/// `crate::test_support` re-exports for the integration tests. That export is
/// `#[doc(hidden)]`, so this is reachable in the type system and invisible in
/// the docs — the same trade `LiveTree` beside it makes.
#[derive(Debug, Default)]
pub struct ChunkCache {
    origin: FsOrigin,
}

impl ChunkCache {
    /// A cache holding nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The leaf row for manifest index `index`, addressing the file if this is
    /// the first time anyone has asked.
    ///
    /// Returns `None` for an index that is out of range or tombstoned, or for a
    /// file that cannot be read. A caller answers `BadIndex` to all of those:
    /// from the far side they are the same thing, *this peer cannot vouch for
    /// that content*.
    pub async fn map_of_index(&self, tree: &LiveTree, index: u32) -> Option<ChunkMap> {
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

        // Already addressed, and the file has not moved underneath. The version
        // gate lives in the store, so a file edited since it was read is
        // unbound here and re-read below rather than answered stale.
        if let Ok(Some(root)) = self.origin.bind(&file).await
            && let Ok(Some(map)) = self.origin.map(root).await
        {
            return Some(map);
        }

        // First ask for this file. Read it once, in chunk-sized pieces, and
        // keep the row.
        //
        // Streamed rather than slurped, which is the improvement over hashing
        // for bao: peak memory is one chunk plus the row, so a 50 GB file costs
        // 64 KiB of buffer instead of 50 GB of it. It happens once per file per
        // version, only for files somebody wants, and never at startup.
        let root = self.origin.adopt(&file, &path).ok()?;
        self.origin.map(root).await.ok().flatten()
    }

    /// The bytes at one address, or `None` if this peer cannot answer for it.
    ///
    /// Scoping is automatic on an origin: the only addresses it knows are ones
    /// it computed from files in this share, so it cannot be used to probe what
    /// the host holds elsewhere. A store shared across shares has to scope
    /// deliberately — see the browser seeder.
    pub async fn chunk(&self, address: ChunkHash) -> Option<Vec<u8>> {
        self.origin.get(address).await.ok().flatten()
    }

    /// Which chunks of `root` this producer can serve.
    ///
    /// # Errors
    /// The underlying source could not answer.
    pub async fn have(&self, root: Root) -> Result<Coverage> {
        self.origin.coverage(root).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use fofoca_chunks::{CHUNK_BYTES_USIZE, chunk_hash};

    use super::ChunkCache;
    use crate::mount::live::LiveTree;

    fn tree_of(root: &std::path::Path) -> Arc<LiveTree> {
        let (manifest, paths) = crate::mount::scan::scan(root).expect("scan");
        Arc::new(LiveTree::new(root.to_path_buf(), manifest, paths))
    }

    #[tokio::test]
    async fn a_row_is_produced_on_demand_and_reused() {
        let data = tempfile::Builder::new()
            .prefix("agent-share-chunks-data-")
            .tempdir()
            .expect("temp dir");
        std::fs::write(data.path().join("a.bin"), vec![7u8; 300_000]).expect("write");
        let tree = tree_of(data.path());
        let cache = ChunkCache::new();

        let map = cache
            .map_of_index(&tree, 0)
            .await
            .expect("index 0 must address");
        assert_eq!(map.len(), 300_000_usize.div_ceil(CHUNK_BYTES_USIZE));

        // Second ask is served from the table. Same answer, and the point of
        // having a cache at all.
        let again = cache.map_of_index(&tree, 0).await.expect("cached");
        assert_eq!(map.root(), again.root());
    }

    /// The bytes behind an address come back, and address what was asked for.
    #[tokio::test]
    async fn a_chunk_can_be_fetched_by_address_alone() {
        let data = tempfile::Builder::new()
            .prefix("agent-share-chunks-chunk-")
            .tempdir()
            .expect("temp dir");
        let body = vec![3u8; CHUNK_BYTES_USIZE + 5];
        std::fs::write(data.path().join("a.bin"), &body).expect("write");
        let tree = tree_of(data.path());
        let cache = ChunkCache::new();

        let map = cache.map_of_index(&tree, 0).await.expect("address");
        assert_eq!(map.len(), 2);
        for index in 0..map.len() {
            let address = map.leaf(index).expect("in range");
            let bytes = cache.chunk(address).await.expect("held");
            assert_eq!(chunk_hash(&bytes), address);
        }
        // An address from nowhere is simply absent.
        assert!(
            cache
                .chunk(chunk_hash(b"not in this share"))
                .await
                .is_none()
        );
    }

    /// **The laziness rule, as a test rather than a comment.** Building a cache
    /// must read nothing: addressing at startup is what would turn `serve` on a
    /// large tree from a `stat` walk into a full read of it.
    #[tokio::test]
    async fn opening_a_cache_addresses_nothing() {
        let data = tempfile::Builder::new()
            .prefix("agent-share-chunks-lazy-data-")
            .tempdir()
            .expect("temp dir");
        let mut expected = Vec::new();
        for name in ["a.bin", "b.bin", "c.bin"] {
            let body = vec![1u8; 100_000];
            std::fs::write(data.path().join(name), &body).expect("write");
            expected.push(chunk_hash(&body[..CHUNK_BYTES_USIZE]));
        }
        let _tree = tree_of(data.path());
        let cache = ChunkCache::new();

        // Not one address is known, so not one file has been read.
        for address in expected {
            assert!(
                cache.chunk(address).await.is_none(),
                "opening a cache must not address anything"
            );
        }
    }

    #[tokio::test]
    async fn an_index_past_the_tree_has_no_row() {
        let data = tempfile::Builder::new()
            .prefix("agent-share-chunks-oob-data-")
            .tempdir()
            .expect("temp dir");
        std::fs::write(data.path().join("a.bin"), b"hi").expect("write");
        let tree = tree_of(data.path());
        let cache = ChunkCache::new();
        assert!(cache.map_of_index(&tree, 424_242).await.is_none());
    }

    /// A file edited after it was addressed must be re-read, not answered from
    /// a row describing content that is gone.
    #[tokio::test]
    async fn an_edited_file_is_readdressed_rather_than_answered_stale() {
        let data = tempfile::Builder::new()
            .prefix("agent-share-chunks-edit-data-")
            .tempdir()
            .expect("temp dir");
        let path = data.path().join("a.bin");
        std::fs::write(&path, vec![1u8; 200_000]).expect("write");
        let tree = tree_of(data.path());
        let cache = ChunkCache::new();
        let before = cache.map_of_index(&tree, 0).await.expect("first");

        // Different content *and* a different size, so the version gate fires
        // on a filesystem whose mtime resolution is coarse.
        std::fs::write(&path, vec![2u8; 200_001]).expect("rewrite");
        let rescanned = tree_of(data.path());
        let after = cache.map_of_index(&rescanned, 0).await.expect("re-address");

        assert_ne!(
            before.root(),
            after.root(),
            "an edited file must not keep the row of its previous content"
        );
        // And the old addresses stop being answerable, so nobody is served a
        // mixture of the two versions.
        assert!(cache.chunk(before.leaf(0).expect("first")).await.is_none());
    }

    /// An empty file is ordinary: it has a row, holds no chunks, and is fully
    /// available to anyone who knows its root.
    #[tokio::test]
    async fn an_empty_file_addresses_cleanly() {
        let data = tempfile::Builder::new()
            .prefix("agent-share-chunks-empty-")
            .tempdir()
            .expect("temp dir");
        std::fs::write(data.path().join("empty.bin"), b"").expect("write");
        let tree = tree_of(data.path());
        let cache = ChunkCache::new();

        let map = cache.map_of_index(&tree, 0).await.expect("address");
        assert_eq!(map.len(), 0);
        assert!(cache.have(map.root()).await.expect("have").is_complete());
    }
}
