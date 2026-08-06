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

use agent_share_proto::PeerCard;
use agent_share_proto::auth::ShareAuth;

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
}

impl SourceSet {
    pub(super) fn new(
        origin: Arc<RemoteClient>,
        endpoint: Endpoint,
        ticket: MountTicket,
        auth: ShareAuth,
        tree: String,
        total_slots: usize,
        cards: Option<super::mesh::CardBook>,
    ) -> Self {
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
        }
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
        let mut out: Vec<PeerCard> = book
            .values()
            .filter(|card| card.endpoint != self.local_endpoint)
            .filter(|card| !struck.contains(&card.endpoint))
            .filter(|card| vouches(card, &self.tree, index, self.total_slots))
            .cloned()
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
        RelayChoice::Pinned => fofoca::RENDEZVOUS_RELAY_LADDER
            .iter()
            .filter_map(|raw| raw.parse().ok())
            .collect(),
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
}
