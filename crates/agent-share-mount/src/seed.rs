//! Serving what this peer happens to hold, on either platform.
//!
//! A viewer that fetched chunks into its store used to *advertise* them and
//! serve nothing — the card said "seeding" while no handler existed to answer a
//! read. This is that listener, and it is generic over the store so the browser
//! serves from `IdbStore` and the CLI from `FsStore` with one implementation
//! between them.
//!
//! Three properties, each load-bearing:
//!
//! - **The origin's envelope is served verbatim.** A seeder is not a second
//!   origin: every index means what the origin says it means, the tree
//!   fingerprint is defined over exactly those bytes, and the creator's
//!   signature is the only authority a seeder has. No peer holds a key to
//!   re-sign with, so re-encoding would leave it unable to prove anything.
//! - **A partial holding is served, not withheld.** A tab or a mount that
//!   fetched half a file answers for that half. This is what a cancelled
//!   download, an abandoned preview and a transfer still in flight all
//!   contribute, and it is the difference between a swarm that survives its
//!   origin and one that does not.
//! - **Answers are scoped to this share.** The chunk store is global — a chunk
//!   addressed by content is the same chunk whichever share it arrived through,
//!   and keeping copies per share would mean fetching the same bytes twice. But
//!   `OP_CHUNK` names no share, so answering *any* address would let anyone
//!   holding one link discover, one address at a time, what else this peer is
//!   storing. A seeder answers only for addresses reachable from a chunk row in
//!   **this** share.
//!
//! # Verification
//!
//! None to do here beyond what the store already does. A chunk proves itself:
//! whoever receives it hashes it and compares against the address they asked
//! for. That is why a partial holder can serve safely, why a chunk can cross
//! shares, and why this file is a fraction of the size of the bao-shaped
//! version it replaces.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

use agent_share_proto::authorship::SignedManifest;
use agent_share_proto::framing::WATCH_FRAME_MANIFEST;
use agent_share_proto::manifest::ReadStatus;
use fofoca_chunks::{ChunkHash, ChunkMap, ChunkSource, Coverage, Root};
use futures::StreamExt as _;
use futures::channel::mpsc;

use crate::serve::{ServeSource, Watcher};

/// What this peer can serve, once it holds something.
struct State<S> {
    /// The origin's `OP_MANIFEST` body, verbatim: `version ‖ signature ‖
    /// manifest`. See the module docs for why the whole envelope is kept.
    envelope: Arc<Vec<u8>>,
    /// Chunk rows for slots this peer knows about, by manifest index.
    rows: HashMap<u32, ChunkMap>,
    /// Root → the slot it describes, for answering `OP_HAVE`.
    slot_of_root: HashMap<Root, u32>,
    /// Every address reachable from a row above.
    ///
    /// **This is the whole of the scoping rule**, and the reason it is a set
    /// rather than a lookup into the store: the store holds chunks from every
    /// share this peer has ever touched, and only these belong to this one.
    in_scope: HashSet<ChunkHash>,
    store: Arc<S>,
}

struct Inner<S> {
    /// `None` until the first chunk lands — a peer that has only browsed serves
    /// nothing and refuses manifest requests rather than answering with a tree
    /// it cannot back.
    state: Option<State<S>>,
    /// Live `OP_WATCH` subscribers. A seeder's stream moves only when its own
    /// snapshot does: it follows the origin while the origin lives and freezes
    /// when it dies, and never fabricates a delta of its own.
    watchers: Vec<mpsc::UnboundedSender<Arc<Vec<u8>>>>,
}

/// A shared handle, cloned into the protocol handler.
///
/// `Arc<RwLock<…>>` rather than the browser's old `Rc<RefCell<…>>` because this
/// type now runs under tokio as well. **No guard is ever held across an await**
/// — every method clones what it needs and drops the lock first — which is what
/// keeps the futures `Send` when the store's are.
pub struct Seeder<S>(Arc<RwLock<Inner<S>>>);

impl<S> Clone for Seeder<S> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<S> std::fmt::Debug for Seeder<S> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Seeder").finish_non_exhaustive()
    }
}

impl<S> Default for Seeder<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> Seeder<S> {
    /// A peer holding nothing, which refuses every request until told otherwise.
    #[must_use]
    pub fn new() -> Self {
        Self(Arc::new(RwLock::new(Inner {
            state: None,
            watchers: Vec::new(),
        })))
    }

    /// Adopt what this peer now knows: the origin's envelope, the chunk rows it
    /// has learned, and the store behind them.
    ///
    /// Called after every fetch, where those are already at hand. Deliberately
    /// takes rows rather than a held-set: what a peer can serve is derived from
    /// the store, chunk by chunk, at the moment somebody asks — so a coverage
    /// figure cached here could never go stale.
    ///
    /// Watchers hear about it only when the envelope actually changed. Holdings
    /// alter what this peer *serves*, not what the tree *is*.
    ///
    /// # Panics
    /// The lock is poisoned, meaning a previous holder panicked mid-update and
    /// the state may be half-written. Serving from it would be worse.
    pub fn update(&self, envelope: Arc<Vec<u8>>, rows: HashMap<u32, ChunkMap>, store: Arc<S>) {
        let mut slot_of_root = HashMap::with_capacity(rows.len());
        let mut in_scope = HashSet::new();
        for (index, row) in &rows {
            slot_of_root.insert(row.root(), *index);
            in_scope.extend(row.leaves().iter().copied());
        }

        let mut inner = self.0.write().expect("the seeder lock is poisoned");
        let changed = inner
            .state
            .as_ref()
            .is_none_or(|state| *state.envelope != *envelope);
        let frame_body = watch_body(&envelope);
        inner.state = Some(State {
            envelope,
            rows,
            slot_of_root,
            in_scope,
            store,
        });
        if changed {
            let mut frame = Vec::with_capacity(1 + frame_body.len());
            frame.push(WATCH_FRAME_MANIFEST);
            frame.extend_from_slice(&frame_body);
            let frame = Arc::new(frame);
            inner
                .watchers
                .retain(|tx| tx.unbounded_send(Arc::clone(&frame)).is_ok());
        }
    }

    /// Whether this peer holds anything at all for this share.
    ///
    /// # Panics
    /// The lock is poisoned.
    #[must_use]
    pub fn is_armed(&self) -> bool {
        self.0
            .read()
            .expect("the seeder lock is poisoned")
            .state
            .is_some()
    }
}

/// The manifest inside an `OP_MANIFEST` envelope, for a watch frame.
///
/// Watch frames carry the bare manifest on every producer — see
/// `LiveTree::opening_frame` for why — so a seeder unwraps rather than passing
/// its envelope through. An envelope that does not decode yields nothing rather
/// than a torn frame; the caller has already accepted it, so this is a shape
/// guard and not a trust decision.
fn watch_body(envelope: &[u8]) -> Vec<u8> {
    SignedManifest::decode(envelope).map_or_else(|_| Vec::new(), |signed| signed.manifest)
}

/// A seeder's watch feed.
#[derive(Debug)]
pub struct Feed(mpsc::UnboundedReceiver<Arc<Vec<u8>>>);

impl Watcher for Feed {
    type Frame = Arc<Vec<u8>>;

    async fn recv(&mut self) -> Option<Self::Frame> {
        self.0.next().await
    }
}

impl<S: ChunkSource + 'static> ServeSource for Seeder<S> {
    type Watcher = Feed;

    fn manifest_envelope(&self) -> Option<Vec<u8>> {
        self.0
            .read()
            .expect("the seeder lock is poisoned")
            .state
            .as_ref()
            .map(|state| state.envelope.as_ref().clone())
    }

    fn subscribe(&self) -> Option<(Vec<u8>, Self::Watcher)> {
        let mut inner = self.0.write().expect("the seeder lock is poisoned");
        let body = watch_body(&inner.state.as_ref()?.envelope);
        let (tx, rx) = mpsc::unbounded();
        inner.watchers.push(tx);
        let mut frame = Vec::with_capacity(1 + body.len());
        frame.push(WATCH_FRAME_MANIFEST);
        frame.extend_from_slice(&body);
        Some((frame, Feed(rx)))
    }

    /// A seeder answers byte ranges too, so an old-style reader and the NFS
    /// lazy mount still work against it. Assembled from the chunks it holds; a
    /// gap means `BadIndex` rather than a short read, because a caller of
    /// `OP_READ` cannot tell a short answer from EOF.
    async fn answer_read(&self, index: u32, offset: u64, len: u32) -> (ReadStatus, Vec<u8>) {
        // Also checked by the dispatch loop, and kept here so the guard holds
        // for a caller that reaches a source directly — the producers do the
        // same. Cheap, and the alternative is a rule that only exists in one
        // place and is silently absent everywhere else.
        if len > agent_share_proto::framing::MAX_READ_LEN {
            return (ReadStatus::LenOverCap, Vec::new());
        }
        // Cloned out, never held across an await: `update` can run while a read
        // is in flight.
        let (row, store) = {
            let inner = self.0.read().expect("the seeder lock is poisoned");
            let Some(state) = inner.state.as_ref() else {
                return (ReadStatus::BadIndex, Vec::new());
            };
            let Some(row) = state.rows.get(&index) else {
                return (ReadStatus::BadIndex, Vec::new());
            };
            (row.clone(), Arc::clone(&state.store))
        };
        if offset >= row.size() || len == 0 {
            return (ReadStatus::Ok, Vec::new());
        }
        let end = offset.saturating_add(u64::from(len)).min(row.size());
        let mut out = Vec::with_capacity(usize::try_from(end - offset).unwrap_or(0));
        let mut cursor = offset;
        while cursor < end {
            let position = row.index_at(cursor);
            let Some(address) = row.leaf(position) else {
                return (ReadStatus::BadIndex, Vec::new());
            };
            let Ok(Some(chunk)) = store.get(address).await else {
                // A hole. Refusing outright is the only honest answer, since a
                // partial `OP_READ` is indistinguishable from end-of-file.
                return (ReadStatus::BadIndex, Vec::new());
            };
            let range = row.range_of(position);
            let within = usize::try_from(cursor - range.start).unwrap_or(0);
            let take = usize::try_from(end - cursor)
                .unwrap_or(usize::MAX)
                .min(chunk.len().saturating_sub(within));
            if take == 0 {
                return (ReadStatus::BadIndex, Vec::new());
            }
            out.extend_from_slice(&chunk[within..within + take]);
            cursor += take as u64;
        }
        (ReadStatus::Ok, out)
    }

    async fn answer_chunk_map(&self, index: u32) -> Option<ChunkMap> {
        let inner = self.0.read().expect("the seeder lock is poisoned");
        inner.state.as_ref()?.rows.get(&index).cloned()
    }

    async fn answer_chunk(&self, address: ChunkHash) -> Option<Vec<u8>> {
        let store = {
            let inner = self.0.read().expect("the seeder lock is poisoned");
            let state = inner.state.as_ref()?;
            // The scoping rule. An address this share does not reference is
            // declined with the same answer as one nobody holds, so the two are
            // indistinguishable from outside.
            if !state.in_scope.contains(&address) {
                return None;
            }
            Arc::clone(&state.store)
        };
        store.get(address).await.ok().flatten()
    }

    async fn answer_have(&self, root: Root) -> Option<Coverage> {
        let store = {
            let inner = self.0.read().expect("the seeder lock is poisoned");
            let state = inner.state.as_ref()?;
            // Same scoping: a peer must not learn what this one holds of a file
            // belonging to a share it was not given.
            state.slot_of_root.get(&root)?;
            Arc::clone(&state.store)
        };
        store.coverage(root).await.ok()
    }
}

#[cfg(test)]
mod tests {
    use super::Seeder;
    use crate::ServeSource as _;
    use fofoca_chunks::{ChunkMap, MemStore, Root, chunk_hash};

    /// Typed once so every case names the same store. `MemStore` rather than a
    /// real backend on purpose: these are about what a seeder *refuses*, which
    /// is decided before any storage is touched — and it lets the browser's
    /// rules be tested on the host, where they never ran before.
    fn empty() -> Seeder<MemStore> {
        Seeder::new()
    }

    /// Before anything is held: no invented manifest, no watch feed, and every
    /// address declined. Advertising happens elsewhere; this is the half that
    /// must never answer for a tree it cannot back.
    #[test]
    fn an_empty_seeder_refuses_everything() {
        let seeder = empty();
        assert!(seeder.manifest_envelope().is_none());
        assert!(seeder.subscribe().is_none());
        futures::executor::block_on(async {
            let (status, bytes) = seeder.answer_read(0, 0, 1024).await;
            assert_eq!(status, agent_share_proto::manifest::ReadStatus::BadIndex);
            assert!(bytes.is_empty());
            assert!(seeder.answer_chunk_map(0).await.is_none());
            assert!(seeder.answer_chunk(chunk_hash(b"anything")).await.is_none());
            assert!(
                seeder
                    .answer_have(ChunkMap::build(b"anything").root())
                    .await
                    .is_none()
            );
        });
    }

    /// The length cap is enforced before any store work, matching the producer.
    #[test]
    fn an_oversized_read_is_capped() {
        futures::executor::block_on(async {
            let (status, _) = empty()
                .answer_read(0, 0, agent_share_proto::framing::MAX_READ_LEN + 1)
                .await;
            assert_eq!(status, agent_share_proto::manifest::ReadStatus::LenOverCap);
        });
    }

    /// A root that belongs to no slot of this share is declined, whatever the
    /// store happens to hold. The probe oracle, closed.
    #[test]
    fn a_root_outside_this_share_is_declined() {
        futures::executor::block_on(async {
            assert!(
                empty()
                    .answer_have(Root::from_bytes([9u8; 32]))
                    .await
                    .is_none()
            );
        });
    }

    /// A seeder holding nothing is not armed, which is what keeps a browsing
    /// peer from advertising a tree it cannot serve.
    #[test]
    fn an_empty_seeder_is_not_armed() {
        assert!(!empty().is_armed());
    }
}
