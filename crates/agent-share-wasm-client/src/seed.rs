//! The seeder half of "every peer a seeder": serve what this tab holds.
//!
//! A viewer that synced files into its store used to *advertise* them on its
//! peer card and serve nothing — the card said "seeding" while no protocol
//! handler existed to answer a read. This module is the missing listener: a
//! [`ServeSource`] backed by the tab's [`IdbStore`], registered on the mesh
//! Router next to the signal handler, exactly the shape the producer uses.
//!
//! Three properties, each load-bearing:
//!
//! - **The origin's manifest bytes are served verbatim.** A seeder is not a
//!   second origin: every READ index means what the origin says it means, and
//!   the tree fingerprint is defined over these exact bytes. Re-encoding would
//!   be a different fingerprint for the same tree — guard #1 would read that
//!   as a diverged peer.
//! - **A slot answers only when held in full** (guard #3). Anything else is
//!   `BadIndex` — "I don't have it" — never a short read, which a consumer
//!   cannot tell from EOF.
//! - **Reads are decoded through the store's own verification.** The store
//!   hands out bao-encoded ranges; decoding them against the bound root means
//!   a corrupted database serves an error, not garbage. The encode→decode
//!   round-trip costs ~2× a hash pass (~GiB/s); a raw-read seam on
//!   `BlobStore` is the upstream optimization if this ever shows up in a
//!   profile.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;

use agent_share_proto::framing::{MAX_READ_LEN, WATCH_FRAME_MANIFEST};
use agent_share_proto::manifest::{MountManifest, ReadStatus};
use anyhow::{Context as _, Result, bail};
use fofoca_blobs::{
    BlobStore as _, CHUNK_GROUP_BYTES, ChunkNum, ChunkRanges, FileId, IdbStore, Outboard,
    SparseBlocks, decode_sparse,
};
use futures::channel::mpsc;

use crate::produce::{ServeSource, WatchFeed};

/// Bytes per bao chunk, fixed by BLAKE3. Byte offsets become chunk numbers
/// through this and nothing else.
const CHUNK_BYTES: u64 = 1024;

/// What this tab can serve, once it holds something.
struct SeederState {
    /// The origin's manifest bytes, verbatim. See the module docs.
    manifest: Rc<Vec<u8>>,
    /// Slot → the store's name for that file, aligned to the origin's `files`
    /// vector. `Some` only for slots held **in full**; tombstones and
    /// never-fetched slots are `None`, which answers `BadIndex`.
    slots: Vec<Option<FileId>>,
    store: Rc<IdbStore>,
}

struct SeederInner {
    /// `None` until the first sync lands — a tab that only browses serves
    /// nothing and refuses manifest requests rather than answering with a
    /// tree it cannot back.
    state: Option<SeederState>,
    /// Live `OP_WATCH` subscribers. A seeder's stream moves only when its own
    /// snapshot does (it follows the origin while the origin lives, and
    /// freezes when it dies); it never fabricates deltas of its own.
    watchers: Vec<mpsc::UnboundedSender<Rc<Vec<u8>>>>,
}

/// Shared handle: one per [`crate::ShareClient`], cloned into the mount
/// handler on the mesh Router.
#[derive(Clone)]
pub(crate) struct SeederShared(Rc<RefCell<SeederInner>>);

impl SeederShared {
    pub(crate) fn new() -> Self {
        Self(Rc::new(RefCell::new(SeederInner {
            state: None,
            watchers: Vec::new(),
        })))
    }

    /// Adopt what the tab now holds: the origin's manifest and the slots held
    /// in full. Called after every sync / held-refresh, right where those
    /// already know all four inputs.
    ///
    /// Notifies watchers only when the manifest bytes actually changed —
    /// held-set changes alter what we *serve*, not what the tree *is*.
    pub(crate) fn update(
        &self,
        manifest_bytes: Rc<Vec<u8>>,
        manifest: &MountManifest,
        store: Rc<IdbStore>,
        held: &BTreeSet<u32>,
    ) {
        let slots = manifest
            .files
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                if entry.is_tombstone() {
                    return None;
                }
                let index = u32::try_from(index).ok()?;
                held.contains(&index).then(|| crate::file_id(entry))
            })
            .collect();

        let mut inner = self.0.borrow_mut();
        let changed = inner
            .state
            .as_ref()
            .is_none_or(|state| *state.manifest != *manifest_bytes);
        inner.state = Some(SeederState {
            manifest: Rc::clone(&manifest_bytes),
            slots,
            store,
        });
        if changed {
            let mut frame = Vec::with_capacity(1 + manifest_bytes.len());
            frame.push(WATCH_FRAME_MANIFEST);
            frame.extend_from_slice(&manifest_bytes);
            let frame = Rc::new(frame);
            inner
                .watchers
                .retain(|tx| tx.unbounded_send(Rc::clone(&frame)).is_ok());
        }
    }
}

impl ServeSource for SeederShared {
    fn manifest_bytes(&self) -> Option<Vec<u8>> {
        self.0
            .borrow()
            .state
            .as_ref()
            .map(|state| state.manifest.as_ref().clone())
    }

    fn subscribe(&self) -> Option<WatchFeed> {
        let mut inner = self.0.borrow_mut();
        let manifest = Rc::clone(&inner.state.as_ref()?.manifest);
        let (tx, rx) = mpsc::unbounded();
        inner.watchers.push(tx);
        let mut frame = Vec::with_capacity(1 + manifest.len());
        frame.push(WATCH_FRAME_MANIFEST);
        frame.extend_from_slice(&manifest);
        Some((frame, rx))
    }

    async fn answer_read(&self, index: u32, offset: u64, len: u32) -> (ReadStatus, Vec<u8>) {
        if len > MAX_READ_LEN {
            return (ReadStatus::LenOverCap, Vec::new());
        }
        // Cloned out, never borrowed across the await below: `update` can run
        // while a read is in flight.
        let (file, store) = {
            let inner = self.0.borrow();
            let Some(state) = inner.state.as_ref() else {
                return (ReadStatus::BadIndex, Vec::new());
            };
            let Some(Some(file)) = state.slots.get(index as usize) else {
                return (ReadStatus::BadIndex, Vec::new());
            };
            (file.clone(), Rc::clone(&state.store))
        };
        match read_window(store.as_ref(), &file, offset, len).await {
            Ok(bytes) => (ReadStatus::Ok, bytes),
            // Guard #3: a seeder that isn't sure says "I don't have it". A
            // version-gate refusal, a hole, or a failed verification all mean
            // the same thing to the reader — this peer cannot answer — and
            // `BadIndex` is the answer that makes it try someone else instead
            // of trusting whatever we could scrape together.
            Err(error) => {
                web_sys::console::debug_1(&wasm_bindgen::JsValue::from_str(&format!(
                    "[seed] read refused (index {index}): {error}"
                )));
                (ReadStatus::BadIndex, Vec::new())
            }
        }
    }
}

/// Read `[offset, offset+len)` of `file` out of the store, verified.
///
/// The store speaks chunk ranges and bao encoding; this maps the byte window
/// onto chunks, decodes against the bound root, and slices the window back
/// out. Reading past the end answers the empty vec, matching the origin.
pub(crate) async fn read_window(
    store: &IdbStore,
    file: &FileId,
    offset: u64,
    len: u32,
) -> Result<Vec<u8>> {
    if offset >= file.size || len == 0 {
        return Ok(Vec::new());
    }
    let end = (offset + u64::from(len)).min(file.size);
    let ranges =
        ChunkRanges::from(ChunkNum(offset / CHUNK_BYTES)..ChunkNum(end.div_ceil(CHUNK_BYTES)));

    // The version gate: a file the store cannot bind any more (it changed, or
    // was never hashed) must not be served from a stale outboard.
    let root = store
        .bind(file)
        .await?
        .context("this file is unbound: never hashed, or changed since")?;
    let encoded = store.read_ranges(file, &ranges).await?;

    let mut blocks = SparseBlocks::empty(file.size, CHUNK_GROUP_BYTES);
    let mut outboard = Outboard::new();
    decode_sparse(
        root,
        file.size,
        &encoded,
        &ranges,
        &mut blocks,
        &mut outboard,
    )?;

    // Reassemble the byte window from the decoded blocks. The decode proved
    // every chunk group covering the window, so a missing block here is a
    // logic error worth failing loudly on, not padding over.
    let map = blocks.into_blocks();
    let mut out = Vec::with_capacity(usize::try_from(end - offset).context("window over usize")?);
    let mut cursor = offset;
    while cursor < end {
        let block_index = cursor / CHUNK_GROUP_BYTES;
        let within = usize::try_from(cursor - block_index * CHUNK_GROUP_BYTES)
            .context("offset within block over usize")?;
        let block = map
            .get(&block_index)
            .context("a decoded block is missing from the window")?;
        let available = block.len().saturating_sub(within);
        if available == 0 {
            bail!("a decoded block is shorter than the window needs");
        }
        let take = usize::try_from(end - cursor)
            .unwrap_or(usize::MAX)
            .min(available);
        out.extend_from_slice(&block[within..within + take]);
        cursor += take as u64;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // wasm32 harness — see `transport_mode.rs` for why.
    use wasm_bindgen_test::wasm_bindgen_test as test;

    /// Guard #3, before the first sync: a tab that holds nothing refuses
    /// everything — no invented manifest, no watch feed, `BadIndex` for any
    /// read. Advertising happens elsewhere; this is the half that must never
    /// answer for a tree it cannot back.
    #[test]
    fn an_empty_seeder_refuses_everything() {
        let seeder = SeederShared::new();
        assert!(seeder.manifest_bytes().is_none());
        assert!(seeder.subscribe().is_none());
        futures::executor::block_on(async {
            let (status, bytes) = seeder.answer_read(0, 0, 1024).await;
            assert_eq!(status, ReadStatus::BadIndex);
            assert!(bytes.is_empty());
        });
    }

    /// The length cap is enforced before any store work: an oversized ask is
    /// `LenOverCap` even on an empty seeder, matching the producer.
    #[test]
    fn an_oversized_read_is_capped() {
        let seeder = SeederShared::new();
        futures::executor::block_on(async {
            let (status, _) = seeder.answer_read(0, 0, MAX_READ_LEN + 1).await;
            assert_eq!(status, ReadStatus::LenOverCap);
        });
    }
}
