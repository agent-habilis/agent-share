//! Multi-source reads: the seam RFC 01 phase 5 named.
//!
//! [`RemoteFs`] reads through [`ByteSource`], and until now the only
//! implementor was one [`RemoteClient`] pinned to the ticket's origin — so a
//! share died with its producer even when a peer on the mesh held a complete
//! copy. [`SourceSet`] is the other implementor: origin first, and when it
//! fails, a peer whose card **vouches** — its `tree` matches the manifest this
//! mount is on and its `serving` covers the slot being read.
//!
//! [`RemoteFs`]: super::nfs::RemoteFs
//!
//! Authority does not move with the bytes. The origin stays the only manifest
//! authority (`watch_tree` holds its own origin client and this set never
//! touches it); a seeder serves a frozen snapshot of the same tree, which the
//! fingerprint filter is what proves.
//!
//! Selection is by marginal cost, not capability: the source that answered
//! last answers next, native (`unicast`) peers are tried before browsers, and
//! a peer that keeps failing is struck out. The origin is always worth
//! retrying last — it coming back is the share going live again.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use async_trait::async_trait;
use fofoca::iroh::{Endpoint, EndpointAddr, TransportAddr};

use agent_share_mount::Seeder;
use agent_share_proto::PeerCard;
use agent_share_proto::auth::ShareAuth;
use fofoca_chunks::{ChunkHash, ChunkMap, ChunkSource as _, ChunkStore as _, FsStore, chunk_hash};

use super::MountTicket;
use super::consume::RemoteClient;
use super::nfs::ByteSource;

/// Strikes before a peer is set aside for this run. Reset only by a card
/// change would be nicer; a run-scoped strikeout is the v1 that cannot serve
/// wrong bytes, only give up on a peer too early.
const MAX_STRIKES: u8 = 3;

/// Who is currently answering reads.
enum Active {
    Origin,
    Peer {
        endpoint_id: String,
        client: Arc<RemoteClient>,
    },
}

/// A [`ByteSource`] over the origin plus every mesh peer that vouches.
pub(super) struct SourceSet {
    origin: Arc<RemoteClient>,
    endpoint: Endpoint,
    /// The origin's ticket — the secret and lookups are the template every
    /// peer client is built from; only the address changes.
    ticket: MountTicket,
    /// What a peer client presents. The mount protocol is symmetric and
    /// authenticates the token alone, with no binding to who is serving, so the
    /// bytes that opened the origin open a seeder too — including on a protected
    /// share, where the password was spent once and never travels again.
    auth: ShareAuth,
    /// Fingerprint of the manifest this mount is on: guard #1's filter.
    tree: String,
    /// Live slot count, the denominator `serving` ranges decode against.
    total_slots: usize,
    /// The mesh roster, shared with [`super::mesh::ShareMesh`]. `None` until
    /// the mesh join resolves (it runs concurrently with the mount coming up)
    /// or when the mesh could not be joined — the set is then origin-only,
    /// which is exactly the behaviour this replaces.
    cards: Mutex<Option<super::mesh::CardBook>>,
    /// Our own endpoint id: never a candidate.
    local_endpoint: String,
    active: tokio::sync::Mutex<Active>,
    strikes: Mutex<HashMap<String, u8>>,
    /// Where bytes this mount reads are kept, so reading is what makes this
    /// peer a seeder. `None` when the store could not be opened, which costs
    /// seeding and never a read.
    store: Option<Arc<FsStore>>,
    /// What this mount serves to others. Fed from `store` and `rows` after
    /// every read that landed something new.
    seeder: Seeder<FsStore>,
    /// Chunk rows for slots this mount has addressed. Both halves of the job:
    /// finding the chunk that covers an offset, and telling the seeder which
    /// addresses belong to *this* share.
    rows: Mutex<HashMap<u32, ChunkMap>>,
    /// The origin's `OP_MANIFEST` body, re-served verbatim — signature
    /// included, since no peer can make another.
    envelope: Arc<Vec<u8>>,
}

/// Everything a [`SourceSet`] is built from.
///
/// A struct because the list crossed what is readable as positional arguments,
/// and because half of them are `Option`s and `Arc`s that would otherwise be
/// distinguishable only by reading the signature.
pub(super) struct SourceSetOpts {
    pub(super) origin: Arc<RemoteClient>,
    pub(super) endpoint: Endpoint,
    pub(super) ticket: MountTicket,
    pub(super) auth: ShareAuth,
    pub(super) tree: String,
    pub(super) total_slots: usize,
    pub(super) cards: Option<super::mesh::CardBook>,
    pub(super) store: Option<Arc<FsStore>>,
    pub(super) envelope: Arc<Vec<u8>>,
    pub(super) seeder: Seeder<FsStore>,
}

impl SourceSet {
    pub(super) fn new(opts: SourceSetOpts) -> Self {
        let SourceSetOpts {
            origin,
            endpoint,
            ticket,
            auth,
            tree,
            total_slots,
            cards,
            store,
            envelope,
            seeder,
        } = opts;
        let local_endpoint = endpoint.id().to_string();
        Self {
            origin,
            endpoint,
            ticket,
            auth,
            tree,
            total_slots,
            cards: Mutex::new(cards),
            local_endpoint,
            active: tokio::sync::Mutex::new(Active::Origin),
            strikes: Mutex::new(HashMap::new()),
            store,
            seeder,
            rows: Mutex::new(HashMap::new()),
            envelope,
        }
    }

    /// Which slots this mount can serve *whole*, for its peer card.
    ///
    /// Asked of the seeder rather than walked here, so a browser tab and a CLI
    /// mount put the same meaning on the wire. `serving` is the whole-slot
    /// contract — `vouches` reads it to pick a peer for an `OP_READ`, which
    /// refuses on any hole — so a slot held in part belongs to
    /// [`agent_share_proto::PeerCard::holding`] and `OP_HAVE`, not here.
    /// Listing it would send readers to bytes that are not there.
    pub(super) async fn serving(&self) -> Option<String> {
        let held = self.seeder.complete_slots().await;
        agent_share_proto::serving::encode_serving(&held, self.total_slots)
    }

    /// Whether this mount holds any chunk at all, for its peer card.
    pub(super) fn holding(&self) -> bool {
        self.seeder.is_armed()
    }

    /// Hand the seeder the row behind bytes this mount just kept.
    ///
    /// Store first, advertise second — the ordering the crash-consistency rule
    /// already follows, and the reason a peer never claims bytes it cannot
    /// serve.
    ///
    /// The first call installs the store and the envelope, which is what arms
    /// the seeder at all. After that a row is adopted on its own: this runs on
    /// every read that fetched anything, and [`Seeder::update`] rebuilds its
    /// scope set from every row it holds, which over a large share is quadratic.
    fn adopt_into_seeder(&self, index: u32, row: &ChunkMap) {
        let Some(store) = self.store.clone() else {
            return;
        };
        if self.seeder.is_armed() {
            self.seeder.adopt(index, row);
            return;
        }
        let rows = self
            .rows
            .lock()
            .map(|rows| rows.clone())
            .unwrap_or_default();
        self.seeder.update(Arc::clone(&self.envelope), rows, store);
    }

    /// The chunk row for `index`, from memory, the store, or the wire.
    async fn row_for(&self, index: u32) -> Option<ChunkMap> {
        if let Ok(rows) = self.rows.lock()
            && let Some(row) = rows.get(&index)
        {
            return Some(row.clone());
        }
        let row = self.origin.fetch_chunk_map(index).await.ok().flatten()?;
        if let Some(store) = self.store.as_ref() {
            let _ = store.put_map(&row).await;
        }
        if let Ok(mut rows) = self.rows.lock() {
            rows.insert(index, row.clone());
        }
        Some(row)
    }

    /// One chunk by address: the store, then the origin, then any peer holding
    /// it. Verified against the address before it is believed.
    /// The bool says whether these bytes were *newly* stored, so a read that
    /// found everything locally does not pay to rebuild the seeder's scope.
    async fn chunk(&self, index: u32, address: ChunkHash) -> Option<(Vec<u8>, bool)> {
        if let Some(store) = self.store.as_ref()
            && let Ok(Some(bytes)) = store.get(address).await
        {
            return Some((bytes, false));
        }
        let fetched = match self.origin.fetch_chunk(address).await {
            Ok(Some(bytes)) => Some(bytes),
            _ => self.chunk_from_peers(index, address).await,
        }?;
        if chunk_hash(&fetched) != address {
            tracing::warn!(
                "a peer answered a chunk address with bytes that address something else"
            );
            return None;
        }
        // **The read is what makes this peer a seeder.** Failing to store is
        // logged and nothing more: a full disk costs the seeding, never the
        // read, exactly as the browser's `keep()` is fire-and-forget.
        if let Some(store) = self.store.as_ref()
            && let Err(error) = store.put(address, &fetched).await
        {
            tracing::debug!(%error, "keeping a chunk failed; not seeding these bytes");
        }
        Some((fetched, true))
    }

    /// Any peer that vouches for this slot, asked by address.
    ///
    /// Guard #3 retires here: a peer holding *part* of the file can answer,
    /// because a chunk request names bytes rather than a whole slot.
    async fn chunk_from_peers(&self, index: u32, address: ChunkHash) -> Option<Vec<u8>> {
        for card in self.candidates(index) {
            let Ok(client) = self.peer_client(&card) else {
                continue;
            };
            if let Ok(Some(bytes)) = client.fetch_chunk(address).await {
                return Some(bytes);
            }
        }
        None
    }

    /// Serve `[offset, offset+len)` out of content-addressed chunks.
    ///
    /// `None` means the chunk path could not answer — no row, or a chunk
    /// nobody would serve — and the caller falls back to ranged reads. That
    /// fallback is why this can be tried first without risking a mount.
    async fn read_chunks(&self, index: u32, offset: u64, len: u32) -> Option<Vec<u8>> {
        let row = self.row_for(index).await?;
        if offset >= row.size() || len == 0 {
            return Some(Vec::new());
        }
        let end = offset.saturating_add(u64::from(len)).min(row.size());
        let mut out = Vec::with_capacity(usize::try_from(end - offset).unwrap_or(0));
        let mut cursor = offset;
        let mut fetched_any = false;
        while cursor < end {
            let position = row.index_at(cursor);
            let address = row.leaf(position)?;
            let (chunk, stored) = self.chunk(index, address).await?;
            fetched_any |= stored;
            let range = row.range_of(position);
            let within = usize::try_from(cursor - range.start).unwrap_or(0);
            let take = usize::try_from(end - cursor)
                .unwrap_or(usize::MAX)
                .min(chunk.len().saturating_sub(within));
            if take == 0 {
                return None;
            }
            out.extend_from_slice(&chunk[within..within + take]);
            cursor += take as u64;
        }
        if fetched_any {
            self.adopt_into_seeder(index, &row);
        }
        Some(out)
    }

    /// Wire the roster in once the mesh join resolves. Idempotent.
    pub(super) fn set_cards(&self, book: super::mesh::CardBook) {
        if let Ok(mut cards) = self.cards.lock() {
            *cards = Some(book);
        }
    }

    /// Peers whose card vouches for `index`, cheapest first.
    fn candidates(&self, index: u32) -> Vec<PeerCard> {
        let cards = match self.cards.lock() {
            Ok(cards) => cards.clone(),
            Err(_) => return Vec::new(),
        };
        let Some(cards) = cards else {
            return Vec::new();
        };
        let Ok(book) = cards.lock() else {
            return Vec::new();
        };
        let struck = self.strikes.lock().map(|strikes| {
            strikes
                .iter()
                .filter(|(_, count)| **count >= MAX_STRIKES)
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>()
        });
        let struck = struck.unwrap_or_default();
        // Every card, present or not. A source hidden because its author
        // went quiet is a stalled download if it turns out to be reachable,
        // and the strike count below already handles one that is not.
        let mut out: Vec<PeerCard> = book
            .all()
            .into_iter()
            .filter(|card| card.endpoint != self.local_endpoint)
            .filter(|card| !struck.contains(&card.endpoint))
            .filter(|card| vouches(card, &self.tree, index, self.total_slots))
            .collect();
        // Native peers first — line rate, no browser in the path. Endpoint id
        // as the tiebreak keeps the order stable across calls.
        out.sort_by_key(|card| (card.transport != "unicast", card.endpoint.clone()));
        out
    }

    fn strike(&self, endpoint_id: &str) {
        if let Ok(mut strikes) = self.strikes.lock() {
            *strikes.entry(endpoint_id.to_owned()).or_default() += 1;
        }
    }

    /// A client for `card`'s peer: the origin's ticket with the address
    /// swapped. Same token — the mount protocol is symmetric, and every mesh
    /// member holds it by definition.
    fn peer_client(&self, card: &PeerCard) -> Result<Arc<RemoteClient>> {
        let id = card
            .endpoint
            .parse::<fofoca::iroh::EndpointId>()
            .context("parsing a peer card's endpoint id")?;
        let ticket = MountTicket {
            addr: seeder_addr(id, &self.ticket.lookups),
            secret: self.ticket.secret,
            lookups: self.ticket.lookups.clone(),
            kind: self.ticket.kind,
            flags: self.ticket.flags,
            mesh_id: self.ticket.mesh_id.clone(),
            author: None,
        };
        Ok(Arc::new(RemoteClient::new(
            self.endpoint.clone(),
            ticket,
            self.auth,
        )))
    }

    /// Try every vouching peer for this read, striking the ones that fail.
    async fn read_from_peers(
        &self,
        index: u32,
        offset: u64,
        len: u32,
    ) -> Option<(String, Arc<RemoteClient>, Vec<u8>)> {
        for card in self.candidates(index) {
            let Ok(client) = self.peer_client(&card) else {
                continue;
            };
            match client.read_range(index, offset, len).await {
                Ok(bytes) => return Some((card.endpoint, client, bytes)),
                Err(error) => {
                    tracing::debug!(peer = %card.endpoint, %error, "seeder read failed");
                    self.strike(&card.endpoint);
                }
            }
        }
        None
    }
}

#[async_trait]
impl ByteSource for SourceSet {
    async fn read(&self, index: u32, offset: u64, len: u32) -> Result<Vec<u8>> {
        // Snapshot who answers first, without holding the lock across the
        // read: reads are concurrent under the NFS bridge, and serializing
        // them behind one mutex would turn readahead into a queue.
        let first = {
            let active = self.active.lock().await;
            match &*active {
                Active::Origin => None,
                Active::Peer {
                    endpoint_id,
                    client,
                } => Some((endpoint_id.clone(), Arc::clone(client))),
            }
        };

        // The chunk path first: content-addressed, verified per chunk, and it
        // *keeps* what it fetched, which is what makes this mount a seeder.
        // `None` means it could not answer — no row, or a chunk nobody would
        // serve — and the ranged path below still can, so trying it costs
        // nothing but is never load-bearing.
        if let Some(bytes) = self.read_chunks(index, offset, len).await {
            return Ok(bytes);
        }

        match first {
            None => match self.origin.read_range(index, offset, len).await {
                Ok(bytes) => Ok(bytes),
                Err(origin_error) => {
                    let Some((id, client, bytes)) = self.read_from_peers(index, offset, len).await
                    else {
                        return Err(origin_error.context(
                            "the origin failed this read and no mesh peer vouches for it",
                        ));
                    };
                    tracing::info!(peer = %id, "origin unreachable; reading from a seeder");
                    *self.active.lock().await = Active::Peer {
                        endpoint_id: id,
                        client,
                    };
                    Ok(bytes)
                }
            },
            Some((active_id, active_client)) => {
                match active_client.read_range(index, offset, len).await {
                    Ok(bytes) => Ok(bytes),
                    Err(peer_error) => {
                        self.strike(&active_id);
                        // The origin coming back is the share going live
                        // again, so it is always retried before the peer pool.
                        if let Ok(bytes) = self.origin.read_range(index, offset, len).await {
                            tracing::info!("the origin answered again; leaving the seeder");
                            *self.active.lock().await = Active::Origin;
                            return Ok(bytes);
                        }
                        let Some((id, client, bytes)) =
                            self.read_from_peers(index, offset, len).await
                        else {
                            return Err(peer_error.context(
                                "the active seeder failed this read, and neither the origin \
                                 nor another vouching peer could answer",
                            ));
                        };
                        *self.active.lock().await = Active::Peer {
                            endpoint_id: id,
                            client,
                        };
                        Ok(bytes)
                    }
                }
            }
        }
    }
}

/// Whether `card` vouches for slot `index` of the tree this mount is on.
///
/// Guard #1 lives here: a card on a different tree — or on none — is not a
/// candidate, however complete its `serving` claims to be, because its
/// indices mean different files. A free function so the filter that keeps
/// wrong bytes out is testable without standing up a mesh.
fn vouches(card: &PeerCard, tree: &str, index: u32, total_slots: usize) -> bool {
    if card.tree.as_deref() != Some(tree) {
        return false;
    }
    card.serving.as_deref().is_some_and(|serving| {
        agent_share_proto::serving::decode_serving(serving, total_slots)
            .binary_search(&index)
            .is_ok()
    })
}

/// The address a seeder of this share can be dialled at: its endpoint id plus
/// the relay ladder the ticket names. Every peer of a share homes on that
/// ladder (the mesh rendezvous uses the same rungs), and for a loopback share
/// the empty list leaves resolution to mDNS/DHT discovery on the endpoint.
pub(super) fn seeder_addr(
    id: fofoca::iroh::EndpointId,
    lookups: &agent_share_proto::lookup::LookupOpts,
) -> EndpointAddr {
    use agent_share_proto::lookup::RelayChoice;
    let relays: Vec<fofoca::iroh::RelayUrl> = match &lookups.relay {
        RelayChoice::Disabled => Vec::new(),
        RelayChoice::Pinned => crate::lookup::pinned_ladder(),
        RelayChoice::Custom(ladder) => ladder.clone(),
    };
    EndpointAddr::from_parts(id, relays.into_iter().map(TransportAddr::Relay))
}

#[cfg(test)]
mod tests {
    use super::vouches;
    use agent_share_proto::PeerCard;

    fn card(tree: Option<&str>, serving: Option<&str>) -> PeerCard {
        PeerCard::new("endpoint-a", "0.1.0", "rust", "unicast", None)
            .with_tree(tree.map(str::to_owned))
            .with_serving(serving.map(str::to_owned))
    }

    /// Guard #1's teeth: a peer on a diverged tree answers plausibly and
    /// wrongly (`a_diverged_peer_answers_plausibly_and_wrongly` pins the
    /// danger), so it must never become a candidate — however complete its
    /// `serving` claims to be.
    #[test]
    fn a_diverged_tree_never_vouches() {
        assert!(!vouches(&card(Some("bbbb"), Some("*")), "aaaa", 0, 3));
    }

    /// A card that says nothing vouches for nothing — absent is "cannot
    /// vouch", not "holds everything".
    #[test]
    fn missing_tree_or_serving_never_vouches() {
        assert!(!vouches(&card(None, Some("*")), "aaaa", 0, 3));
        assert!(!vouches(&card(Some("aaaa"), None), "aaaa", 0, 3));
    }

    /// `serving` must cover the slot being read: a partial mirror vouches for
    /// what it holds and nothing else.
    #[test]
    fn serving_must_cover_the_slot() {
        let partial = card(Some("aaaa"), Some("0-1"));
        assert!(vouches(&partial, "aaaa", 1, 4));
        assert!(!vouches(&partial, "aaaa", 2, 4));
        let complete = card(Some("aaaa"), Some("*"));
        assert!(vouches(&complete, "aaaa", 3, 4));
    }

    /// A seeder is dialled on the very rungs [`crate::lookup::pinned_ladder`]
    /// hands the endpoint. The two used to parse `RENDEZVOUS_RELAY_LADDER`
    /// separately — and disagreed on a bad rung, one dropping it and the other
    /// panicking — so this pins that they stay one list.
    ///
    /// Compared as sets: `EndpointAddr` keeps its addresses sorted, so rung
    /// order does not survive the trip. Nothing downstream wants it to — iroh
    /// picks a home relay by measured latency, never by position.
    #[test]
    fn a_pinned_seeder_is_dialled_on_the_endpoint_ladder() {
        use agent_share_proto::lookup::LookupOpts;

        let id = fofoca::iroh::SecretKey::generate().public();
        let addr = super::seeder_addr(id, &LookupOpts::public_preset());

        let mut dialled: Vec<_> = addr.relay_urls().cloned().collect();
        let mut ladder = crate::lookup::pinned_ladder();
        dialled.sort();
        ladder.sort();
        assert_eq!(dialled, ladder);
        assert!(!dialled.is_empty(), "a public share must name its rungs");
    }

    /// A loopback share names no rungs: resolution is left to mDNS/DHT on the
    /// endpoint, and a stray relay address would send it off-box.
    #[test]
    fn a_loopback_seeder_names_no_relay() {
        use agent_share_proto::lookup::LookupOpts;

        let id = fofoca::iroh::SecretKey::generate().public();
        let addr = super::seeder_addr(id, &LookupOpts::loopback());

        assert_eq!(addr.relay_urls().count(), 0);
    }
}
