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
    /// Where to report a slot this peer was asked for and could not complete.
    ///
    /// One owner — whoever published the card that made the promise. See
    /// [`Seeder::holes`].
    holes: Option<mpsc::UnboundedSender<u32>>,
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
            holes: None,
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
        let frame_body = envelope.as_ref().clone();
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

    /// Adopt one row, leaving everything else as it stands.
    ///
    /// **This is what lets a chunk be seeded the moment it lands.** A chunk is
    /// addressed by `blake3` of its own bytes, so holding one is enough to serve
    /// one — but [`Self::answer_chunk`] answers only for addresses reachable
    /// from a row this seeder knows, and a downloader that waited for its whole
    /// transfer to finish before saying so would be invisible for exactly the
    /// window it has bytes worth asking for. Called with the row *before* its
    /// chunks are fetched, every one of them is servable as it arrives.
    ///
    /// [`Self::update`] rebuilds the scope set from every row it is given, which
    /// is O(all chunks) — per file over a large share that is quadratic. This
    /// costs one row.
    ///
    /// A no-op until [`Self::update`] has supplied a store and an envelope:
    /// there is nothing to answer *with* yet, and a row alone would not change
    /// that.
    ///
    /// [`Self::answer_chunk`]: crate::ServeSource::answer_chunk
    ///
    /// # Panics
    /// The lock is poisoned.
    pub fn adopt(&self, index: u32, row: &ChunkMap) {
        let mut inner = self.0.write().expect("the seeder lock is poisoned");
        let Some(state) = inner.state.as_mut() else {
            return;
        };
        state.slot_of_root.insert(row.root(), index);
        state.in_scope.extend(row.leaves().iter().copied());
        state.rows.insert(index, row.clone());
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

    /// Slots this peer was asked to serve and could not complete.
    ///
    /// Serving is honest on its own: a hole answers `BadIndex` rather than a
    /// short read, and `OP_HAVE` is derived from the store at the moment it is
    /// asked. What cannot correct itself is the *card* — `serving` is a set
    /// published earlier, and a store the browser evicted under quota pressure
    /// leaves it claiming bytes this peer no longer has. This is the feed that
    /// tells its owner to look again.
    ///
    /// A report is not proof of loss: a partial holding is ordinary, and reading
    /// into a hole of a slot nobody ever advertised is an ordinary refusal. The
    /// owner decides, by checking that slot against the store — which is why
    /// this carries the slot and no verdict.
    ///
    /// **Only `OP_READ` reports.** `OP_CHUNK` sees the same evidence but cannot
    /// name a slot — [`State::in_scope`] is a flat set of addresses — and the
    /// swarm reaches it through `OP_HAVE`, which is re-derived from the store
    /// per request and so cannot go stale. The card is the only stale claim, and
    /// a reader consults it before `OP_HAVE`, so a peer that is never read from
    /// keeps a stale card until it is. That is the known cost of reporting from
    /// the read path rather than polling; the CLI takes the other side of the
    /// trade in `consume::spawn_serving_updates`, where a 5 s re-derive is cheap
    /// against a local filesystem and would not be against `IndexedDB`.
    ///
    /// One receiver at a time; subscribing again drops the previous one.
    ///
    /// # Panics
    /// The lock is poisoned.
    #[must_use]
    pub fn holes(&self) -> mpsc::UnboundedReceiver<u32> {
        let (tx, rx) = mpsc::unbounded();
        self.0.write().expect("the seeder lock is poisoned").holes = Some(tx);
        rx
    }

    /// Report a slot this peer promised and could not deliver. Never blocks, and
    /// never fails: an unsubscribed or dropped feed simply has no owner to tell.
    fn report_hole(&self, index: u32) {
        let inner = self.0.read().expect("the seeder lock is poisoned");
        if let Some(sender) = inner.holes.as_ref() {
            let _ = sender.unbounded_send(index);
        }
    }

    /// A handle that does **not** keep this seeder alive.
    ///
    /// The retraction watcher outlives nothing: it holds one of these, so a
    /// client that is dropped and replaced — which a redial does — takes its
    /// rows, its address set and its store with it instead of leaving them
    /// pinned by a task that will never be polled again.
    #[must_use]
    pub fn downgrade(&self) -> WeakSeeder<S> {
        WeakSeeder(Arc::downgrade(&self.0))
    }
}

/// A [`Seeder`] handle that holds no claim on the state behind it.
pub struct WeakSeeder<S>(std::sync::Weak<RwLock<Inner<S>>>);

impl<S> std::fmt::Debug for WeakSeeder<S> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("WeakSeeder").finish_non_exhaustive()
    }
}

impl<S> WeakSeeder<S> {
    /// The seeder, or `None` once every strong handle is gone.
    #[must_use]
    pub fn upgrade(&self) -> Option<Seeder<S>> {
        self.0.upgrade().map(Seeder)
    }
}

impl<S: ChunkSource> Seeder<S> {
    /// Which slots this peer holds *in full*, asked of the store right now.
    ///
    /// The card's `serving` set is exactly this, and deriving it here rather
    /// than caching it is the same rule [`Seeder::update`] follows: a coverage
    /// figure kept in memory is a claim that can outlive the bytes it describes.
    ///
    /// One store round trip per row, so this belongs on the paths that already
    /// walk the whole share. To re-check a single slot, use [`Self::holds`].
    ///
    /// # Panics
    /// The lock is poisoned.
    pub async fn complete_slots(&self) -> Vec<u32> {
        // Snapshotted, never held across the awaits below.
        let (roots, store) = {
            let inner = self.0.read().expect("the seeder lock is poisoned");
            let Some(state) = inner.state.as_ref() else {
                return Vec::new();
            };
            let roots: Vec<(u32, Root, usize)> = state
                .rows
                .iter()
                .map(|(index, row)| (*index, row.root(), row.len()))
                .collect();
            (roots, Arc::clone(&state.store))
        };
        // One question for the whole share rather than one per file: on the
        // browser's store a per-root answer probes that root's chunks, and this
        // asks about all of them at once — see `ChunkSource::coverage_of`.
        //
        // Counted against the row for the reason [`complete`] gives: an empty
        // coverage is what a store answers for a root it has never seen, and
        // `is_complete` cannot tell that from a zero-chunk file.
        let addresses: Vec<Root> = roots.iter().map(|(_, root, _)| *root).collect();
        let Ok(coverages) = store.coverage_of(&addresses).await else {
            return Vec::new();
        };
        let mut held: Vec<u32> = roots
            .iter()
            .zip(coverages)
            .filter(|((_, _, chunks), coverage)| coverage.count() == *chunks)
            .map(|((index, _, _), _)| *index)
            .collect();
        held.sort_unstable();
        held
    }

    /// Whether this peer still holds all of one slot.
    ///
    /// The narrow question behind [`Self::holes`]: a report names a slot, so
    /// answering it costs one coverage lookup rather than a walk of the share.
    ///
    /// # Panics
    /// The lock is poisoned.
    pub async fn holds(&self, index: u32) -> bool {
        let Some((root, chunks, store)) = ({
            let inner = self.0.read().expect("the seeder lock is poisoned");
            inner.state.as_ref().and_then(|state| {
                let row = state.rows.get(&index)?;
                Some((row.root(), row.len(), Arc::clone(&state.store)))
            })
        }) else {
            return false;
        };
        complete(store.as_ref(), root, chunks).await
    }
}

/// Whether `store` holds every one of a row's `chunks`.
///
/// Counted against the row rather than asked [`Coverage::is_complete`], which is
/// true of an empty coverage — and a store that has never heard of a root
/// answers exactly that. The two are indistinguishable to `is_complete` and
/// opposite in meaning: a zero-chunk file really is fully held, while an evicted
/// chunk *map* holds nothing at all.
async fn complete<S: ChunkSource>(store: &S, root: Root, chunks: usize) -> bool {
    store
        .coverage(root)
        .await
        .is_ok_and(|coverage| coverage.count() == chunks)
}

/// The manifest inside an `OP_MANIFEST` envelope, for a watch frame.
///
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
        // Passed through exactly as handed over: a seeder carries a version the
        // creator signed and can mint none of its own.
        let body = inner.state.as_ref()?.envelope.as_ref().clone();
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
                self.report_hole(index);
                return (ReadStatus::BadIndex, Vec::new());
            };
            let Ok(Some(chunk)) = store.get(address).await else {
                // A hole. Refusing outright is the only honest answer, since a
                // partial `OP_READ` is indistinguishable from end-of-file.
                //
                // Reported as well as refused: the read is correct either way,
                // but if this slot is on our card the card is now a lie.
                self.report_hole(index);
                return (ReadStatus::BadIndex, Vec::new());
            };
            let range = row.range_of(position);
            let within = usize::try_from(cursor - range.start).unwrap_or(0);
            let take = usize::try_from(end - cursor)
                .unwrap_or(usize::MAX)
                .min(chunk.len().saturating_sub(within));
            if take == 0 {
                self.report_hole(index);
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
    use std::sync::Arc;

    use fofoca_chunks::{ChunkMap, MemStore, Root, chunk_hash};

    use super::Seeder;
    use crate::ServeSource as _;

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

    /// An armed seeder over `store`, serving `bytes` at slot 7.
    ///
    /// The envelope is a real [`SignedManifest`] because `update` unwraps one
    /// for its watch frame; its contents do not matter to any case here.
    fn armed(store: Arc<MemStore>, bytes: &[u8]) -> (Seeder<MemStore>, ChunkMap) {
        use agent_share_proto::authorship::{SIGNATURE_LEN, SignedManifest};

        let row = ChunkMap::build(bytes);
        let envelope = SignedManifest {
            version: 1,
            signature: [0u8; SIGNATURE_LEN],
            manifest: agent_share_proto::manifest::MountManifest::default().encode(),
        }
        .encode();
        let seeder = Seeder::new();
        let mut rows = std::collections::HashMap::new();
        rows.insert(7u32, row.clone());
        seeder.update(Arc::new(envelope), rows, store);
        (seeder, row)
    }

    /// The eviction signal. The browser drops `IndexedDB` under quota pressure
    /// without asking, so a slot the card claims can stop being backed by
    /// anything. Refusing the read is already right; reporting it is what lets
    /// the owner take the claim back off the card.
    #[test]
    fn a_hole_reports_the_slot_it_refused() {
        futures::executor::block_on(async {
            // A row whose chunks were never stored: what an eviction leaves.
            let (seeder, _) = armed(Arc::new(MemStore::new()), b"the bytes of a file");
            let mut holes = seeder.holes();
            let (status, _) = seeder.answer_read(7, 0, 8).await;
            assert_eq!(status, agent_share_proto::manifest::ReadStatus::BadIndex);
            assert_eq!(holes.try_recv().ok(), Some(7), "the slot was not reported");
        });
    }

    /// A slot this peer never knew is refused in silence. Only a *broken
    /// promise* is worth reporting, and there was no promise here — reporting it
    /// would spend a coverage walk on every probe of an unknown index.
    #[test]
    fn an_unknown_slot_is_refused_without_a_report() {
        futures::executor::block_on(async {
            let (seeder, _) = armed(Arc::new(MemStore::new()), b"the bytes of a file");
            let mut holes = seeder.holes();
            let (status, _) = seeder.answer_read(99, 0, 8).await;
            assert_eq!(status, agent_share_proto::manifest::ReadStatus::BadIndex);
            assert_eq!(holes.try_recv().ok(), None, "an unknown slot was reported");
        });
    }

    /// What the card is allowed to claim, and the trap underneath it: a store
    /// that has never heard of a root answers with an *empty* coverage, and
    /// `is_complete` is true of that. Counting against the row tells the
    /// zero-chunk file apart from the evicted chunk map.
    #[test]
    fn complete_slots_asks_the_store_and_not_the_row() {
        futures::executor::block_on(async {
            use fofoca_chunks::ChunkStore as _;

            let bytes = b"the bytes of a file";
            let store = Arc::new(MemStore::new());
            let (seeder, row) = armed(Arc::clone(&store), bytes);
            assert!(
                seeder.complete_slots().await.is_empty(),
                "a row with no stored map or chunks is not a holding"
            );

            store.put_map(&row).await.expect("put the map");
            for position in 0..row.len() {
                let range = row.range_of(position);
                let start = usize::try_from(range.start).expect("a test file fits usize");
                let end = usize::try_from(range.end).expect("a test file fits usize");
                let slice = &bytes[start..end];
                let address = row.leaf(position).expect("leaf");
                store.put(address, slice).await.expect("put the chunk");
            }
            assert_eq!(
                seeder.complete_slots().await,
                vec![7],
                "a fully stored slot is servable"
            );
        });
    }

    /// The narrow re-check behind a hole report. It has to agree with
    /// `complete_slots` — two rules for "fully held" would flap, each undoing
    /// the other's card — and it must not claim a slot it has no row for.
    #[test]
    fn holds_answers_for_one_slot_only() {
        futures::executor::block_on(async {
            use fofoca_chunks::ChunkStore as _;

            let bytes = b"the bytes of a file";
            let store = Arc::new(MemStore::new());
            let (seeder, row) = armed(Arc::clone(&store), bytes);
            assert!(!seeder.holds(7).await, "nothing is stored yet");
            assert!(!seeder.holds(99).await, "a slot with no row is never held");

            store.put_map(&row).await.expect("put the map");
            for position in 0..row.len() {
                let range = row.range_of(position);
                let start = usize::try_from(range.start).expect("a test file fits usize");
                let end = usize::try_from(range.end).expect("a test file fits usize");
                let address = row.leaf(position).expect("leaf");
                store.put(address, &bytes[start..end]).await.expect("put");
            }
            assert!(seeder.holds(7).await);
            assert_eq!(
                seeder.holds(7).await,
                seeder.complete_slots().await.contains(&7),
                "the narrow check and the walk must agree"
            );
        });
    }

    /// **One chunk is enough to seed one chunk.**
    ///
    /// The claim the whole partial-seeding design rests on: a peer that has
    /// fetched part of a file answers for the part it has, reports it through
    /// `OP_HAVE`, and is still honest that it cannot serve the slot whole. A
    /// downloader is therefore useful to the swarm from its first chunk rather
    /// than from its last.
    #[test]
    fn one_chunk_is_enough_to_seed_that_chunk() {
        futures::executor::block_on(async {
            use fofoca_chunks::ChunkStore as _;

            // Two chunks, so "some" and "all" are genuinely different.
            let bytes = vec![7u8; fofoca_chunks::CHUNK_BYTES_USIZE + 1];
            let store = Arc::new(MemStore::new());
            let (seeder, row) = armed(Arc::clone(&store), &bytes);
            assert!(row.len() >= 2, "the fixture must span more than one chunk");

            store.put_map(&row).await.expect("put the map");
            let first = row.leaf(0).expect("leaf");
            let range = row.range_of(0);
            let end = usize::try_from(range.end).expect("a test file fits usize");
            store.put(first, &bytes[..end]).await.expect("put");

            assert_eq!(
                seeder.answer_chunk(first).await.as_deref(),
                Some(&bytes[..end]),
                "a held chunk must be served, whole file or not"
            );
            let coverage = seeder.answer_have(row.root()).await.expect("in scope");
            assert_eq!(
                coverage.count(),
                1,
                "OP_HAVE must report the one held chunk"
            );
            assert!(
                seeder.complete_slots().await.is_empty(),
                "the card must not claim a slot that cannot be read whole"
            );
            assert!(seeder.is_armed(), "a partial holder still holds something");
        });
    }

    /// `adopt` is what makes a chunk servable the moment it lands: before it the
    /// address is out of scope, and an out-of-scope address is refused exactly
    /// like one nobody holds.
    #[test]
    fn a_row_is_not_servable_until_it_is_adopted() {
        futures::executor::block_on(async {
            use fofoca_chunks::ChunkStore as _;

            let bytes = b"the bytes of a file";
            let store = Arc::new(MemStore::new());
            let (seeder, _) = armed(Arc::clone(&store), b"a different file");

            // A second file, stored but never adopted.
            let row = ChunkMap::build(bytes);
            store.put_map(&row).await.expect("put the map");
            let address = row.leaf(0).expect("leaf");
            store.put(address, bytes).await.expect("put");
            assert!(
                seeder.answer_chunk(address).await.is_none(),
                "an unadopted address is outside this share's scope"
            );

            seeder.adopt(9, &row);
            assert_eq!(
                seeder.answer_chunk(address).await.as_deref(),
                Some(&bytes[..]),
                "adopting the row brings its chunks into scope"
            );
            assert_eq!(seeder.complete_slots().await, vec![9]);
        });
    }

    /// The watcher must not be what keeps a retired client's rows, address set
    /// and store alive.
    #[test]
    fn a_weak_handle_does_not_keep_the_seeder_alive() {
        let (seeder, _) = armed(Arc::new(MemStore::new()), b"the bytes of a file");
        let weak = seeder.downgrade();
        assert!(weak.upgrade().is_some());
        drop(seeder);
        assert!(weak.upgrade().is_none(), "the seeder was pinned by a Weak");
    }
}
