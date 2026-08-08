//! Who is on a share's mesh, read off the meta document and the engine's
//! roster together.
//!
//! Two sources have to meet here, and they are keyed differently on purpose.
//! The meta CRDT document is keyed by **nickname**, because the engine's
//! per-peer write gate only lets a peer write under its own author name. The
//! roster of who is actually present is keyed by nickname too. But a *card*
//! identifies its peer by **endpoint id**, which is what a mount session can
//! be addressed by, and which survives the rejoin that mints a fresh nickname.
//!
//! So: join on the nickname, key the result by the endpoint. An earlier
//! attempt at this filtered an endpoint-keyed book against a nickname-keyed
//! roster — a comparison that is empty by construction, which read as "every
//! peer has left" and hid live ones. The nickname is *evidence*; the endpoint
//! is *identity*; neither substitutes for the other.
//!
//! Deliberately free of the engine: this takes plain nicknames and cards, so
//! it is testable without standing a node up, and it carries no clock, which
//! is what keeps it usable from `wasm32-unknown-unknown` (`Instant::now`
//! panics there).

use std::collections::{BTreeMap, BTreeSet};

use crate::client::PeerCard;

/// One `/peers/<nick>/card` entry, with the name it was published under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetaEntry {
    /// The author nickname the document keys this card by. Random per join
    /// and not unique, so it is only ever used to look the peer up in the
    /// roster — never as identity.
    pub nickname: String,
    pub card: PeerCard,
}

/// Read every `/peers/<nick>/card` out of a meta document.
///
/// Tolerant on purpose, and each tolerance costs exactly one peer rather than
/// the roster: an entry with no card yet (a peer that joined but has not
/// published), a card this build cannot parse (a peer on an older or newer
/// shape), and a card explicitly set to `null` (the leave-time retraction a
/// departing peer writes over its own entry) are all skipped.
#[must_use]
pub fn entries_from_meta(doc: &serde_json::Value) -> Vec<MetaEntry> {
    let Some(peers) = doc.get("peers").and_then(serde_json::Value::as_object) else {
        return Vec::new();
    };
    peers
        .iter()
        .filter_map(|(nickname, peer)| {
            let card = PeerCard::from_card_value(peer.get("card")?)?;
            Some(MetaEntry {
                nickname: nickname.clone(),
                card,
            })
        })
        .collect()
}

/// The cards of peers the engine currently counts as present.
///
/// `live` is the set of nicknames on the roster with the idle ones already
/// removed — the same set the engine's own peer count is taken from, which is
/// what makes a list built here agree with that count instead of merely
/// resembling it.
///
/// A card whose nickname is not on the roster is dropped. That covers a peer
/// that left, one evicted for silence, and the historical entries anti-entropy
/// hands a fresh joiner — all without a timer of our own, because the engine
/// has already made that judgement and we are reading it rather than repeating
/// it. The converse, a roster nickname that has published no card, yields
/// nothing here; a peer we cannot describe is not a row.
///
/// One peer occupying two nicknames collapses to one entry, which is the
/// desired reading: it is one peer. The survivor is chosen by nickname order
/// so that two callers, or one caller twice, agree — a plain map insert over
/// an unordered document picks an arbitrary winner that changes between
/// rebuilds.
#[must_use]
pub fn live_cards(entries: &[MetaEntry], live: &BTreeSet<String>) -> Vec<PeerCard> {
    let mut by_endpoint: BTreeMap<&str, (&str, &PeerCard)> = BTreeMap::new();
    for entry in entries {
        if !live.contains(entry.nickname.as_str()) {
            continue;
        }
        by_endpoint
            .entry(entry.card.endpoint.as_str())
            .and_modify(|held| {
                if entry.nickname.as_str() < held.0 {
                    *held = (entry.nickname.as_str(), &entry.card);
                }
            })
            .or_insert((entry.nickname.as_str(), &entry.card));
    }
    by_endpoint
        .into_values()
        .map(|(_, card)| card.clone())
        .collect()
}

/// What a mesh driver publishes for readers outside its event loop.
///
/// Two views of one snapshot, and which one a caller wants is a real choice.
/// A peer list wants [`Self::present`]: a card whose author the engine no
/// longer counts is a peer that is gone, and showing it is the defect this
/// split exists to close. A reader looking for bytes wants [`Self::all`]:
/// hiding a source that turns out to be alive costs a stalled download, which
/// is far worse than one stale row.
///
/// Held by both clients. The driver lives inside the engine's event loop and
/// is unreachable from the outside, so the loop writes here and everything
/// else reads.
#[derive(Default, Debug)]
pub struct Roster {
    /// Every `/peers/<nick>/card` in the meta document, departed peers
    /// included. Nothing deletes an entry, so this only grows.
    entries: Vec<MetaEntry>,
    /// Nicknames the engine counts as present, with the idle ones already
    /// dropped — the same set its peer count is taken from, which is what
    /// makes a list built here agree with that count.
    present: BTreeSet<String>,
}

impl Roster {
    #[must_use]
    pub fn new(entries: Vec<MetaEntry>, present: BTreeSet<String>) -> Self {
        Self { entries, present }
    }

    /// Cards of the peers that are here.
    #[must_use]
    pub fn present(&self) -> Vec<PeerCard> {
        live_cards(&self.entries, &self.present)
    }

    /// Every card the document holds, present or not.
    #[must_use]
    pub fn all(&self) -> Vec<PeerCard> {
        self.entries
            .iter()
            .map(|entry| entry.card.clone())
            .collect()
    }

    /// The card describing `endpoint`, from anywhere in the document.
    ///
    /// Searches [`Self::all`] rather than the present half, because this
    /// decorates a row that already exists for its own reasons — our own, the
    /// producer's, a live data channel's. Those rows do not come from the
    /// roster, so letting the roster blank their version and runtime would
    /// turn a peer we are talking to right now into an unnamed one.
    #[must_use]
    pub fn card_for(&self, endpoint: &str) -> Option<PeerCard> {
        self.entries
            .iter()
            .find(|entry| entry.card.endpoint == endpoint)
            .map(|entry| entry.card.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::{MetaEntry, Roster, entries_from_meta, live_cards};
    use crate::client::PeerCard;
    use std::collections::BTreeSet;

    fn card(endpoint: &str) -> PeerCard {
        PeerCard::new(
            endpoint,
            "0.1.0",
            "rust",
            "unicast",
            Some("consumer".to_owned()),
        )
    }

    fn entry(nickname: &str, endpoint: &str) -> MetaEntry {
        MetaEntry {
            nickname: nickname.to_owned(),
            card: card(endpoint),
        }
    }

    fn live(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    fn meta(entries: &[(&str, serde_json::Value)]) -> serde_json::Value {
        let peers: serde_json::Map<String, serde_json::Value> = entries
            .iter()
            .map(|(nick, body)| ((*nick).to_owned(), body.clone()))
            .collect();
        serde_json::json!({ "peers": peers })
    }

    /// The nickname is the join key, so it has to survive the parse. The
    /// previous readers threw it away and re-keyed by endpoint, which left
    /// nothing to compare a nickname-keyed roster against.
    #[test]
    fn the_parse_keeps_the_name_a_card_was_published_under() {
        let doc = meta(&[(
            "alice",
            serde_json::json!({ "card": card("ep-a").to_card_value() }),
        )]);
        let entries = entries_from_meta(&doc);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].nickname, "alice");
        assert_eq!(entries[0].card.endpoint, "ep-a");
    }

    /// Every tolerance costs one peer and never the roster.
    #[test]
    fn a_peer_this_build_cannot_read_costs_only_that_peer() {
        let doc = meta(&[
            ("no-card-yet", serde_json::json!({})),
            (
                "retracted",
                serde_json::json!({ "card": serde_json::Value::Null }),
            ),
            (
                "gibberish",
                serde_json::json!({ "card": { "nonsense": true } }),
            ),
            (
                "alice",
                serde_json::json!({ "card": card("ep-a").to_card_value() }),
            ),
        ]);
        let entries = entries_from_meta(&doc);
        let names: Vec<&str> = entries
            .iter()
            .map(|entry| entry.nickname.as_str())
            .collect();
        assert_eq!(names, vec!["alice"]);
    }

    /// An empty or shapeless document is an empty roster, not a panic.
    #[test]
    fn a_document_with_no_peers_yields_nothing() {
        assert!(entries_from_meta(&serde_json::json!({})).is_empty());
        assert!(entries_from_meta(&serde_json::json!({ "peers": 7 })).is_empty());
    }

    /// The fix, stated directly: presence comes from the roster, so a card
    /// belonging to a peer the engine no longer counts is not a row. This is
    /// what stops the list growing forever while the count stays right.
    #[test]
    fn a_card_whose_peer_is_gone_from_the_roster_is_dropped() {
        let entries = [entry("alice", "ep-a"), entry("ghost", "ep-gone")];
        let cards = live_cards(&entries, &live(&["alice"]));
        let endpoints: Vec<&str> = cards.iter().map(|card| card.endpoint.as_str()).collect();
        assert_eq!(endpoints, vec!["ep-a"]);
    }

    /// And the converse, so the filter cannot be mistaken for a whitelist: a
    /// peer on the roster that has published nothing yet is not a row either.
    #[test]
    fn a_roster_name_with_no_card_yields_nothing() {
        assert!(live_cards(&[], &live(&["alice"])).is_empty());
    }

    /// Two nicknames, one machine. It is one peer, and both readings of the
    /// document must agree on which card describes it — an unordered insert
    /// picks a winner that changes between rebuilds.
    #[test]
    fn one_peer_under_two_nicknames_collapses_the_same_way_every_time() {
        let forwards = [entry("alpha", "ep-a"), entry("omega", "ep-a")];
        let backwards = [entry("omega", "ep-a"), entry("alpha", "ep-a")];
        let names = live(&["alpha", "omega"]);
        let one = live_cards(&forwards, &names);
        let other = live_cards(&backwards, &names);
        assert_eq!(one.len(), 1, "two names for one endpoint is one peer");
        assert_eq!(one, other, "the survivor must not depend on document order");
    }

    /// Nothing on the roster means nothing to show — not "show everything".
    #[test]
    fn an_empty_roster_shows_nobody() {
        let entries = [entry("alice", "ep-a")];
        assert!(live_cards(&entries, &BTreeSet::new()).is_empty());
    }

    fn book() -> Roster {
        Roster::new(
            vec![entry("alice", "ep-a"), entry("ghost", "ep-gone")],
            live(&["alice"]),
        )
    }

    /// The whole point of the split: one snapshot answers both questions
    /// differently, so a caller picks by what it is about to do.
    #[test]
    fn the_two_views_disagree_about_a_peer_that_left() {
        let endpoints = |cards: Vec<PeerCard>| {
            cards
                .into_iter()
                .map(|card| card.endpoint)
                .collect::<Vec<_>>()
        };
        assert_eq!(endpoints(book().present()), vec!["ep-a"]);
        assert_eq!(endpoints(book().all()), vec!["ep-a", "ep-gone"]);
    }

    /// A row we hold a live connection to must keep its name even when gossip
    /// has written the peer off. This is why `card_for` reads `all`: the row
    /// exists because of the connection, and blanking its version and runtime
    /// would report a peer we are actively talking to as unknown.
    #[test]
    fn a_card_still_describes_a_peer_the_roster_has_dropped() {
        assert_eq!(
            book().card_for("ep-gone").map(|card| card.endpoint),
            Some("ep-gone".to_owned())
        );
        assert!(book().card_for("ep-never-seen").is_none());
    }

    /// A driver that has not run yet answers "nobody", not a panic — the
    /// browser seeds one of these before its event loop starts.
    #[test]
    fn a_book_nobody_has_filled_in_is_empty() {
        let empty = Roster::default();
        assert!(empty.present().is_empty());
        assert!(empty.all().is_empty());
        assert!(empty.card_for("ep-a").is_none());
    }
}
