//! What the CLI answers mount requests from.
//!
//! The dispatch loop itself lives in [`agent_share_mount`], shared with the
//! browser. This is the CLI's half of that seam: a producer reading through to
//! files on disk, addressed lazily by [`super::hash::ChunkCache`].
//!
//! There is nothing platform-specific about the *protocol* here, and that is
//! the point — everything below is about `std::fs` paths and tokio broadcast,
//! which is exactly what could not be shared and therefore all that is left.

use std::sync::Arc;
use std::time::Duration;

use agent_share_mount::{ServeSource, Watcher};
use agent_share_proto::framing::ManifestSince;
use agent_share_proto::manifest::ReadStatus;
use fofoca_chunks::{ChunkHash, ChunkMap, Coverage, Root};
use tokio::sync::broadcast;

use super::hash::ChunkCache;
use super::live::LiveTree;

/// A share served from the filesystem it was scanned from.
///
/// Built per stream, which costs two `Arc` clones — the tree and the chunk
/// table are shared, never copied.
#[derive(Debug, Clone)]
pub(super) struct ProducerSource {
    tree: Arc<LiveTree>,
    /// `None` is a supported state, not a degraded one: a producer that keeps
    /// no chunk table answers `BadIndex` to every address, and the consumer
    /// falls back to reading ranges. The tests exercise it.
    hashes: Option<Arc<ChunkCache>>,
}

impl ProducerSource {
    pub(super) fn new(tree: Arc<LiveTree>, hashes: Option<Arc<ChunkCache>>) -> Self {
        Self { tree, hashes }
    }
}

/// A watcher's end of the tree's broadcast, plus the tree itself so a lagging
/// consumer can be resent the whole manifest.
#[derive(Debug)]
pub(super) struct TreeWatcher {
    updates: broadcast::Receiver<Arc<Vec<u8>>>,
    tree: Arc<LiveTree>,
}

impl Watcher for TreeWatcher {
    /// The tree's own `Arc`, so a frame crosses this seam without being copied.
    /// On a 74k-file share a full-manifest frame is several MB.
    type Frame = Arc<Vec<u8>>;

    async fn recv(&mut self) -> Option<Self::Frame> {
        match self.updates.recv().await {
            Ok(frame) => Some(frame),
            Err(broadcast::error::RecvError::Lagged(missed)) => {
                // Deltas only mean anything applied in order and in full, so a
                // consumer that missed some cannot be caught up with the next
                // one. Resend the whole tree instead.
                tracing::debug!(missed, "watcher fell behind; resending the manifest");
                Some(Arc::new(self.tree.opening_frame()))
            }
            Err(broadcast::error::RecvError::Closed) => None,
        }
    }
}

impl ServeSource for ProducerSource {
    type Watcher = TreeWatcher;

    fn manifest_envelope(&self) -> Option<Vec<u8>> {
        Some(self.tree.manifest_envelope().as_ref().clone())
    }

    /// The one source that can answer this: it publishes the changes, so it is
    /// the only one with a difference to describe.
    fn answer_manifest_since(&self, since: u64) -> Option<ManifestSince> {
        let (target_version, signature, deltas) = self.tree.deltas_since(since)?;
        Some(ManifestSince {
            target_version,
            signature,
            deltas,
        })
    }

    fn subscribe(&self) -> Option<(Vec<u8>, Self::Watcher)> {
        // Subscribe *before* snapshotting the manifest: the other order would
        // drop any change landing in between, and the consumer would never hear
        // of it.
        let updates = self.tree.subscribe();
        let opening = self.tree.opening_frame();
        Some((
            opening,
            TreeWatcher {
                updates,
                tree: Arc::clone(&self.tree),
            },
        ))
    }

    async fn answer_read(&self, index: u32, offset: u64, len: u32) -> (ReadStatus, Vec<u8>) {
        super::produce::answer_read(&self.tree, index, offset, len).await
    }

    async fn answer_chunk_map(&self, index: u32) -> Option<ChunkMap> {
        self.hashes.as_ref()?.map_of_index(&self.tree, index).await
    }

    async fn answer_chunk(&self, address: ChunkHash) -> Option<Vec<u8>> {
        // Scoping is automatic on an origin: the only addresses it knows are
        // ones it computed from files in *this* share, so answering by address
        // alone cannot be used to probe what the host holds elsewhere. A store
        // shared across shares has to scope deliberately — see the seeder.
        self.hashes.as_ref()?.chunk(address).await
    }

    async fn answer_have(&self, root: Root) -> Option<Coverage> {
        self.hashes.as_ref()?.have(root).await.ok()
    }

    /// `finish` only *marks* a stream done, so on a fast connection — loopback
    /// especially — the teardown can race ahead of the last bytes. Wait for the
    /// consumer's acknowledgement, bounded, so a peer that never sends one
    /// costs two seconds rather than a stuck task.
    async fn settle(&self, send: fofoca::iroh::endpoint::SendStream) {
        let _ = tokio::time::timeout(Duration::from_secs(2), send.stopped()).await;
    }
}

/// The two things the CLI can serve from, as one concrete type.
///
/// An enum rather than a generic because `tokio::spawn` needs the whole future
/// to be `Send`, and with async-fn-in-trait that cannot be *stated* about a
/// type parameter without unstable return-type notation. Naming both sources
/// concretely lets the compiler see the `Send` it already has, and keeps one
/// spawned task per stream — the parallelism a mount's readahead depends on.
///
/// The browser needs no equivalent: its futures are `!Send` by construction and
/// it spawns them locally.
#[derive(Debug, Clone)]
pub(super) enum NativeSource {
    /// A share read through to the files it was scanned from.
    Producer(ProducerSource),
    /// A mount serving what it has read into its own chunk store.
    Seeding(agent_share_mount::Seeder<fofoca_chunks::FsStore>),
}

/// The matching watcher, for the same reason.
#[derive(Debug)]
pub(super) enum NativeWatcher {
    Tree(TreeWatcher),
    Seeding(agent_share_mount::Feed),
}

impl Watcher for NativeWatcher {
    type Frame = Arc<Vec<u8>>;

    async fn recv(&mut self) -> Option<Self::Frame> {
        match self {
            Self::Tree(watcher) => watcher.recv().await,
            Self::Seeding(feed) => feed.recv().await,
        }
    }
}

impl ServeSource for NativeSource {
    type Watcher = NativeWatcher;

    fn manifest_envelope(&self) -> Option<Vec<u8>> {
        match self {
            Self::Producer(source) => source.manifest_envelope(),
            Self::Seeding(seeder) => seeder.manifest_envelope(),
        }
    }

    /// Forwarded rather than left to the trait's default, which would refuse on
    /// the one source that can answer. The seeder keeps the default on purpose:
    /// it re-serves a snapshot and publishes no changes of its own.
    fn answer_manifest_since(&self, since: u64) -> Option<ManifestSince> {
        match self {
            Self::Producer(source) => source.answer_manifest_since(since),
            Self::Seeding(seeder) => seeder.answer_manifest_since(since),
        }
    }

    fn subscribe(&self) -> Option<(Vec<u8>, Self::Watcher)> {
        match self {
            Self::Producer(source) => source
                .subscribe()
                .map(|(frame, watcher)| (frame, NativeWatcher::Tree(watcher))),
            Self::Seeding(seeder) => seeder
                .subscribe()
                .map(|(frame, feed)| (frame, NativeWatcher::Seeding(feed))),
        }
    }

    async fn answer_read(&self, index: u32, offset: u64, len: u32) -> (ReadStatus, Vec<u8>) {
        match self {
            Self::Producer(source) => source.answer_read(index, offset, len).await,
            Self::Seeding(seeder) => seeder.answer_read(index, offset, len).await,
        }
    }

    async fn answer_chunk_map(&self, index: u32) -> Option<ChunkMap> {
        match self {
            Self::Producer(source) => source.answer_chunk_map(index).await,
            Self::Seeding(seeder) => seeder.answer_chunk_map(index).await,
        }
    }

    async fn answer_chunk(&self, address: ChunkHash) -> Option<Vec<u8>> {
        match self {
            Self::Producer(source) => source.answer_chunk(address).await,
            Self::Seeding(seeder) => seeder.answer_chunk(address).await,
        }
    }

    async fn answer_have(&self, root: Root) -> Option<Coverage> {
        match self {
            Self::Producer(source) => source.answer_have(root).await,
            Self::Seeding(seeder) => seeder.answer_have(root).await,
        }
    }

    async fn settle(&self, send: fofoca::iroh::endpoint::SendStream) {
        match self {
            Self::Producer(source) => source.settle(send).await,
            Self::Seeding(seeder) => seeder.settle(send).await,
        }
    }
}
