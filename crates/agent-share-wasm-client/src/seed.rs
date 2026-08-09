//! The seeder half of "every peer a seeder": serve what this tab holds.
//!
//! A viewer that fetched chunks into its store used to *advertise* them on its
//! peer card and serve nothing — the card said "seeding" while no protocol
//! handler existed to answer a read. This module is that listener: a
//! [`ServeSource`] backed by the tab's [`IdbStore`], registered on the mesh
//! Router next to the signal handler, exactly the shape the producer uses.
//!
//! Three properties, each load-bearing:
//!
//! - **The origin's manifest bytes are served verbatim.** A seeder is not a
//!   second origin: every index means what the origin says it means, and the
//!   tree fingerprint is defined over these exact bytes. Re-encoding would be a
//!   different fingerprint for the same tree, which reads as a diverged peer.
//! - **A partial holding is served, not withheld.** A tab that fetched half a
//!   file answers for that half. This is what a cancelled download, an
//!   abandoned preview and a transfer still in flight all contribute, and it is
//!   the difference between a swarm that survives its origin and one that does
//!   not. There is no short read to be confused with EOF here, because a chunk
//!   is asked for by address and answered whole or not at all.
//! - **Answers are scoped to this share.** The chunk store is global — a chunk
//!   addressed by content is the same chunk whichever share it arrived through,
//!   and keeping copies per share would mean fetching the same bytes twice. But
//!   `OP_CHUNK` names no file and no share, so answering *any* address would let
//!   anyone holding one link discover, one address at a time, what else this tab
//!   is storing. So a seeder answers only for addresses reachable from a chunk
//!   row in **this** share.
//!
//! # Verification
//!
//! There is none to do here beyond what the store already does. A chunk proves
//! itself: whoever receives it hashes it and compares against the address they
//! asked for. That is why a partial holder can serve safely, why a chunk can
//! cross shares, and why this file is a fraction of the size of the bao-shaped
//! version it replaces.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use agent_share_proto::framing::{MAX_READ_LEN, WATCH_FRAME_MANIFEST};
use agent_share_proto::authorship::SignedManifest;
use agent_share_proto::manifest::ReadStatus;
use fofoca_chunks::{ChunkHash, ChunkMap, ChunkSource as _, Coverage, IdbStore, Root};
use futures::channel::mpsc;

use crate::produce::{ServeSource, WatchFeed};

/// What this tab can serve, once it holds something.
struct SeederState {
    /// The origin's `OP_MANIFEST` body, verbatim: `version ‖ signature ‖
    /// manifest`. See the module docs.
    ///
    /// Kept whole rather than reduced to the manifest, because the signature is
    /// the only authority a seeder has. This tab cannot sign — no browser holds
    /// the creator's key — so a seeder that stored only the manifest could
    /// serve the right bytes and still be unable to prove they were the
    /// creator's.
    envelope: Rc<Vec<u8>>,
    /// Chunk rows for slots this tab knows about, by manifest index.
    rows: HashMap<u32, ChunkMap>,
    /// Root → the slot it describes, for answering `OP_HAVE`.
    slot_of_root: HashMap<Root, u32>,
    /// Every address reachable from a row above.
    ///
    /// **This is the whole of the scoping rule**, and the reason it is a set
    /// rather than a lookup into the store: the store holds chunks from every
    /// share this browser has ever touched, and only these belong to this one.
    in_scope: HashSet<ChunkHash>,
    store: Rc<IdbStore>,
}

struct SeederInner {
    /// `None` until the first chunk lands — a tab that only browses serves
    /// nothing and refuses manifest requests rather than answering with a tree
    /// it cannot back.
    state: Option<SeederState>,
    /// Live `OP_WATCH` subscribers. A seeder's stream moves only when its own
    /// snapshot does (it follows the origin while the origin lives, and freezes
    /// when it dies); it never fabricates deltas of its own.
    watchers: Vec<mpsc::UnboundedSender<Rc<Vec<u8>>>>,
}

/// Shared handle: one per [`crate::ShareClient`], cloned into the mount handler
/// on the mesh Router.
#[derive(Clone)]
pub(crate) struct SeederShared(Rc<RefCell<SeederInner>>);

impl SeederShared {
    pub(crate) fn new() -> Self {
        Self(Rc::new(RefCell::new(SeederInner {
            state: None,
            watchers: Vec::new(),
        })))
    }

    /// Adopt what this tab now knows: the origin's manifest, the chunk rows it
    /// has learned, and the store behind them.
    ///
    /// Called after every fetch, right where those are already known.
    /// Deliberately takes rows rather than a held-set: what this tab can serve
    /// is derived from the store, chunk by chunk, at the moment somebody asks —
    /// so a coverage figure cached here could never go stale.
    ///
    /// Notifies watchers only when the manifest bytes actually changed —
    /// holdings alter what we *serve*, not what the tree *is*.
    pub(crate) fn update(
        &self,
        envelope: Rc<Vec<u8>>,
        rows: HashMap<u32, ChunkMap>,
        store: Rc<IdbStore>,
    ) {
        let mut slot_of_root = HashMap::with_capacity(rows.len());
        let mut in_scope = HashSet::new();
        for (index, row) in &rows {
            slot_of_root.insert(row.root(), *index);
            in_scope.extend(row.leaves().iter().copied());
        }

        let mut inner = self.0.borrow_mut();
        let changed = inner
            .state
            .as_ref()
            .is_none_or(|state| *state.envelope != *envelope);
        let frame_body = watch_body(&envelope);
        inner.state = Some(SeederState {
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
            let frame = Rc::new(frame);
            inner
                .watchers
                .retain(|tx| tx.unbounded_send(Rc::clone(&frame)).is_ok());
        }
    }
}

/// The manifest inside an `OP_MANIFEST` envelope, for a watch frame.
///
/// Watch frames carry the bare manifest on every producer — see
/// `mount::live::LiveTree::opening_frame` for why — so a seeder unwraps rather
/// than passing its envelope through. An envelope that does not decode yields
/// nothing rather than a torn frame; the caller has already accepted it, so
/// this is a shape guard and not a trust decision.
fn watch_body(envelope: &[u8]) -> Vec<u8> {
    SignedManifest::decode(envelope).map_or_else(|_| Vec::new(), |signed| signed.manifest)
}

impl ServeSource for SeederShared {
    fn manifest_envelope(&self) -> Option<Vec<u8>> {
        self.0
            .borrow()
            .state
            .as_ref()
            .map(|state| state.envelope.as_ref().clone())
    }

    fn subscribe(&self) -> Option<WatchFeed> {
        let mut inner = self.0.borrow_mut();
        let body = watch_body(&inner.state.as_ref()?.envelope);
        let (tx, rx) = mpsc::unbounded();
        inner.watchers.push(tx);
        let mut frame = Vec::with_capacity(1 + body.len());
        frame.push(WATCH_FRAME_MANIFEST);
        frame.extend_from_slice(&body);
        Some((frame, rx))
    }

    /// A seeder answers byte ranges too, so an old-style reader and the NFS
    /// lazy mount still work against it. Assembled from the chunks it holds; a
    /// gap means `BadIndex` rather than a short read, because a caller of
    /// `OP_READ` cannot tell a short answer from EOF.
    async fn answer_read(&self, index: u32, offset: u64, len: u32) -> (ReadStatus, Vec<u8>) {
        if len > MAX_READ_LEN {
            return (ReadStatus::LenOverCap, Vec::new());
        }
        // Cloned out, never borrowed across an await: `update` can run while a
        // read is in flight.
        let (row, store) = {
            let inner = self.0.borrow();
            let Some(state) = inner.state.as_ref() else {
                return (ReadStatus::BadIndex, Vec::new());
            };
            let Some(row) = state.rows.get(&index) else {
                return (ReadStatus::BadIndex, Vec::new());
            };
            (row.clone(), Rc::clone(&state.store))
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
        let inner = self.0.borrow();
        inner.state.as_ref()?.rows.get(&index).cloned()
    }

    async fn answer_chunk(&self, address: ChunkHash) -> Option<Vec<u8>> {
        let store = {
            let inner = self.0.borrow();
            let state = inner.state.as_ref()?;
            // The scoping rule. An address this share does not reference is
            // declined with the same answer as one nobody holds, so the two are
            // indistinguishable from outside.
            if !state.in_scope.contains(&address) {
                return None;
            }
            Rc::clone(&state.store)
        };
        store.get(address).await.ok().flatten()
    }

    async fn answer_have(&self, root: Root) -> Option<Coverage> {
        let store = {
            let inner = self.0.borrow();
            let state = inner.state.as_ref()?;
            // Same scoping: a peer must not learn what this tab holds of a file
            // belonging to a share it was not given.
            state.slot_of_root.get(&root)?;
            Rc::clone(&state.store)
        };
        store.coverage(root).await.ok()
    }
}

#[cfg(test)]
mod tests {
    use super::SeederShared;
    use crate::produce::ServeSource as _;
    use fofoca_chunks::{ChunkMap, Root, chunk_hash};

    // wasm32 harness — see `transport_mode.rs` for why.
    use wasm_bindgen_test::wasm_bindgen_test as test;

    /// Before anything is held: no invented manifest, no watch feed, and every
    /// address declined. Advertising happens elsewhere; this is the half that
    /// must never answer for a tree it cannot back.
    #[test]
    fn an_empty_seeder_refuses_everything() {
        let seeder = SeederShared::new();
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
        let seeder = SeederShared::new();
        futures::executor::block_on(async {
            let (status, _) = seeder
                .answer_read(0, 0, agent_share_proto::framing::MAX_READ_LEN + 1)
                .await;
            assert_eq!(status, agent_share_proto::manifest::ReadStatus::LenOverCap);
        });
    }

    /// A root that belongs to no slot of this share is declined, whatever the
    /// store happens to hold. The probe oracle, closed.
    #[test]
    fn a_root_outside_this_share_is_declined() {
        let seeder = SeederShared::new();
        futures::executor::block_on(async {
            let elsewhere = Root::from_bytes([9u8; 32]);
            assert!(seeder.answer_have(elsewhere).await.is_none());
        });
    }
}
