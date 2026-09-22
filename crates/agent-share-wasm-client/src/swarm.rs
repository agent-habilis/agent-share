//! Which peer serves which chunk, and in what order.
//!
//! A browser peer used to download from exactly one source: `connection` was
//! singular, and a second holder of the same bytes was only ever *failover*.
//! This module is the scheduling half of pulling from several at once.
//!
//! # Rarest first
//!
//! Chunks are ordered by how many peers hold them, fewest first. This is
//! BitTorrent's core heuristic and it is not about speed — it is about what
//! survives. The chunk one peer holds is the one that disappears when that peer
//! closes its tab; fetching it first is how a second copy comes to exist while
//! there is still somebody to ask. Fetching the common chunks first would leave
//! the whole swarm converging on the same easy bytes and the rare ones
//! one-tab-deep for as long as the transfer lasts.
//!
//! # Pinned to a root, never to a slot
//!
//! Every peer here answered `OP_HAVE` for **one root**, so a plan describes one
//! version of one file. If the origin edits that file mid-download the new root
//! is a different download: this one finishes from peers still holding `R`, or
//! it fails. It must never mix, and it cannot, because a chunk that arrives is
//! checked against the address the plan asked for.
//!
//! # Why this is a pure function
//!
//! Scheduling is where a download quietly goes wrong — everything assigned to
//! one peer, a rare chunk left for last, a position nobody holds silently
//! dropped — and none of that is visible in a browser. Taking coverages in and
//! handing assignments back means the interesting part is decided by code a unit
//! test can drive, and the async half has nothing left to get wrong but I/O.

use fofoca_chunks::Coverage;

/// One peer's share of the work: the positions it was asked for, in the order
/// they should be requested.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PeerPlan {
    /// Index into the `holders` slice the plan was built from.
    pub(crate) peer: usize,
    /// Chunk positions, rarest first.
    pub(crate) positions: Vec<usize>,
}

/// What the swarm can and cannot cover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Plan {
    /// Per-peer queues. Peers that were assigned nothing are left out.
    pub(crate) peers: Vec<PeerPlan>,
    /// Positions **no** peer advertised, in the order they were wanted.
    ///
    /// Never silently dropped: the caller falls back to the connection it is
    /// homed on, which is the only source that might hold a chunk nobody
    /// admits to. A plan that quietly omitted these would read as a complete
    /// download that stops short.
    pub(crate) unheld: Vec<usize>,
}

/// Assign `missing` across the peers whose coverage is given, rarest first.
///
/// `holders[i]` is what peer `i` answered for this root. A peer whose coverage
/// is shorter than the row it claims to describe is not special-cased: a
/// position past its end simply reads as "not held", which is the safe
/// direction — it costs one fallback, never a wrong chunk.
///
/// Within a rarity tier the least-loaded holder wins, so two chunks held by the
/// same single peer both go to it while a chunk held by three is spread. Ties
/// break on the lower peer index, so a plan is deterministic and a test can
/// assert on it.
pub(crate) fn plan(missing: &[usize], holders: &[Coverage]) -> Plan {
    let mut ranked: Vec<(usize, Vec<usize>)> = missing
        .iter()
        .map(|&position| {
            let who: Vec<usize> = holders
                .iter()
                .enumerate()
                .filter(|(_, coverage)| coverage.contains(position))
                .map(|(peer, _)| peer)
                .collect();
            (position, who)
        })
        .collect();

    // Rarest first, and `missing`'s own order as the tiebreak so a file still
    // arrives roughly front-to-back among chunks of equal rarity. A stable sort
    // is what preserves that second property.
    ranked.sort_by_key(|(_, who)| who.len());

    let mut load = vec![0usize; holders.len()];
    let mut queues: Vec<Vec<usize>> = vec![Vec::new(); holders.len()];
    let mut unheld = Vec::new();
    for (position, who) in ranked {
        let Some(&peer) = who.iter().min_by_key(|&&peer| (load[peer], peer)) else {
            unheld.push(position);
            continue;
        };
        load[peer] += 1;
        queues[peer].push(position);
    }
    // Back to wanted-order, since rarity said nothing about these.
    unheld.sort_unstable();

    Plan {
        peers: queues
            .into_iter()
            .enumerate()
            .filter(|(_, positions)| !positions.is_empty())
            .map(|(peer, positions)| PeerPlan { peer, positions })
            .collect(),
        unheld,
    }
}

#[cfg(test)]
mod tests {
    use fofoca_chunks::Coverage;
    // The crate only builds for wasm32, so its harness is wasm-bindgen's. The
    // alias keeps the tests looking like ordinary ones; `live_state` does the
    // same.
    use wasm_bindgen_test::wasm_bindgen_test as test;

    use super::{PeerPlan, plan};

    /// A peer holding exactly `held` of a `len`-chunk file.
    fn peer(len: usize, held: &[usize]) -> Coverage {
        let mut coverage = Coverage::empty(len);
        for &position in held {
            coverage.insert(position);
        }
        coverage
    }

    /// The heuristic, as an assertion. Chunk 2 is held by one peer and chunk 0
    /// by three, so 2 is requested first — that is the whole point, and getting
    /// it backwards would be invisible in every other test.
    #[test]
    fn the_rarest_chunk_is_asked_for_first() {
        let holders = [peer(3, &[0, 1, 2]), peer(3, &[0, 1]), peer(3, &[0])];
        let out = plan(&[0, 1, 2], &holders);
        let first = out
            .peers
            .iter()
            .find(|p| p.positions.contains(&2))
            .expect("chunk 2 is assigned");
        assert_eq!(first.peer, 0, "only peer 0 holds chunk 2");
        assert_eq!(first.positions.first(), Some(&2), "and it is asked first");
    }

    /// A position nobody advertised must surface, not vanish. This is the
    /// difference between a download that falls back and one that stops short
    /// and calls itself finished.
    #[test]
    fn a_chunk_no_peer_holds_is_reported_rather_than_dropped() {
        let holders = [peer(4, &[0, 1])];
        let out = plan(&[0, 1, 2, 3], &holders);
        assert_eq!(out.unheld, vec![2, 3]);
        let assigned: Vec<usize> = out.peers.iter().flat_map(|p| p.positions.clone()).collect();
        assert_eq!(assigned.len(), 2);
    }

    /// With no peers at all, everything falls back and nothing is lost.
    #[test]
    fn an_empty_swarm_plans_nothing_and_keeps_everything() {
        let out = plan(&[0, 1, 2], &[]);
        assert!(out.peers.is_empty());
        assert_eq!(out.unheld, vec![0, 1, 2]);
    }

    /// Work is spread rather than piled on the first capable peer.
    #[test]
    fn equally_rare_chunks_are_shared_between_holders() {
        let holders = [peer(4, &[0, 1, 2, 3]), peer(4, &[0, 1, 2, 3])];
        let out = plan(&[0, 1, 2, 3], &holders);
        assert_eq!(out.peers.len(), 2);
        for peer_plan in &out.peers {
            assert_eq!(peer_plan.positions.len(), 2, "an even split");
        }
        assert!(out.unheld.is_empty());
    }

    /// But a peer that is the *only* holder gets all of its own chunks, however
    /// lopsided that makes the split.
    #[test]
    fn a_sole_holder_takes_all_of_what_only_it_has() {
        let holders = [peer(4, &[0, 1, 2]), peer(4, &[3])];
        let out = plan(&[0, 1, 2, 3], &holders);
        let zero = out.peers.iter().find(|p| p.peer == 0).expect("peer 0");
        assert_eq!(zero.positions.len(), 3);
        let one = out.peers.iter().find(|p| p.peer == 1).expect("peer 1");
        assert_eq!(one.positions, vec![3]);
    }

    /// Among chunks of equal rarity the file still arrives roughly in order,
    /// which matters for a preview that renders as it streams.
    #[test]
    fn equal_rarity_keeps_the_wanted_order() {
        let holders = [peer(4, &[0, 1, 2, 3])];
        let out = plan(&[0, 1, 2, 3], &holders);
        assert_eq!(out.peers[0].positions, vec![0, 1, 2, 3]);
    }

    /// A peer that answered for a shorter row cannot be asked for positions
    /// past its end. Reading that as "not held" costs a fallback; reading it as
    /// held would cost a stalled download.
    #[test]
    fn a_short_coverage_never_claims_the_tail() {
        let holders = [peer(2, &[0, 1])];
        let out = plan(&[0, 1, 5], &holders);
        assert_eq!(out.unheld, vec![5]);
        assert_eq!(
            out.peers,
            vec![PeerPlan {
                peer: 0,
                positions: vec![0, 1]
            }]
        );
    }

    /// Nothing missing is nothing to do — the common case once a file is held,
    /// and it must not dial anybody.
    #[test]
    fn nothing_missing_plans_nothing() {
        let out = plan(&[], &[peer(4, &[0, 1, 2, 3])]);
        assert!(out.peers.is_empty());
        assert!(out.unheld.is_empty());
    }
}
