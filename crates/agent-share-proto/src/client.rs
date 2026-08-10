//! Share peer identity published on the mesh **meta** channel.
//!
//! All mesh/app metadata a peer wants others to see goes under
//! `/peers/<nick>/card` via `broadcast_state_merge(Channel::Meta)`. Session-local
//! UI state (mount progress, ICE observations, wall-clock connected-for) stays
//! out of the card.
//!
//! Browser consumers build the structured fields in `TypeScript` and pass them
//! into wasm; the CLI fills them itself. Display label:
//! `agent-share v1.2.3 (chrome, webrtc)`.

use serde::{Deserialize, Serialize};

/// Product name that leads every client label.
pub const PRODUCT: &str = "agent-share";

/// Meta-card field: full display label (derived; also stored for simple readers).
pub const CARD_CLIENT: &str = "client";
/// Meta-card field: peer endpoint id.
pub const CARD_ENDPOINT: &str = "endpoint";
/// Meta-card field: product name (`agent-share`).
pub const CARD_APP: &str = "app";
/// Meta-card field: semver without the leading `v`.
pub const CARD_VERSION: &str = "version";
/// Meta-card field: runtime (`chrome` / `safari` / `rust` / …).
pub const CARD_RUNTIME: &str = "runtime";
/// Meta-card field: data-plane transport (`webrtc` / `relay` / `unicast`).
pub const CARD_TRANSPORT: &str = "transport";
/// Meta-card field: share role (`producer` / `consumer`), when known.
pub const CARD_ROLE: &str = "role";
/// Meta-card field: fingerprint of the manifest this peer is on, when known.
///
/// See [`crate::manifest::manifest_fingerprint`]. Two peers agreeing here are
/// on the same tree, so their file indices mean the same thing; two that
/// disagree are not interchangeable as sources, because a `READ` addresses a
/// file by its position in the manifest and those positions have diverged.
pub const CARD_TREE: &str = "tree";
/// Meta-card field: which manifest slots this peer can serve, when known.
///
/// See [`crate::serving`]. `"*"` for every live slot, else sorted run-length
/// ranges. Absent means *cannot vouch* — a peer that has not worked it out, or
/// one whose availability was too scattered to fit the frame.
pub const CARD_SERVING: &str = "serving";

/// Card key for [`PeerCard::holding`].
pub const CARD_HOLDING: &str = "holding";

/// Structured identity written to `/peers/<nick>/card`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerCard {
    pub endpoint: String,
    pub app: String,
    pub version: String,
    pub runtime: String,
    pub transport: String,
    /// Full label: `agent-share v{version} ({runtime}, {transport})`.
    pub client: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Fingerprint of the manifest this peer is serving or reading, when known.
    ///
    /// Optional because a peer legitimately does not know it yet: the browser
    /// joins the share mesh inside its client constructor, before it has
    /// fetched any manifest, so it publishes its card with no tree and fills it
    /// in afterwards. To a reader `None` means *cannot vouch for this peer's
    /// tree*, so it is not a candidate source — but it is not an error either.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree: Option<String>,
    /// Which manifest slots this peer can serve, when it knows.
    ///
    /// Only meaningful alongside a matching [`Self::tree`]: an index means
    /// nothing without agreeing which manifest it indexes into.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serving: Option<String>,
    /// Whether this peer holds **any** chunk of this share.
    ///
    /// [`Self::serving`] answers a different question — which slots can be read
    /// *whole* — and a peer part-way through a transfer answers `None` to it
    /// while being a perfectly good chunk source: a chunk is addressed by the
    /// hash of its own bytes, so holding one is enough to serve one. Readers
    /// pick whole-file sources off `serving` and chunk sources off this.
    ///
    /// Deliberately a flag and not a bitfield. Per-chunk availability changes as
    /// every range lands, and this rides a CRDT that keeps history — see
    /// [`crate::serving`]. The detail is answered live by `OP_HAVE`; this only
    /// says whether it is worth the round trip.
    ///
    /// `None` means *unknown*, not *nothing*: a peer built before this field
    /// omits it, and reading that as "holds nothing" would make every older peer
    /// invisible. Fall back to [`Self::serving`] being present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holding: Option<bool>,
}

impl PeerCard {
    /// Build a card from parts. `client` is derived via [`format_label`].
    #[must_use]
    pub fn new(
        endpoint: impl Into<String>,
        version: impl Into<String>,
        runtime: impl Into<String>,
        transport: impl Into<String>,
        role: Option<String>,
    ) -> Self {
        let version = version.into();
        let runtime = runtime.into();
        let transport = transport.into();
        let client = format_label(&version, &runtime, &transport);
        Self {
            endpoint: endpoint.into(),
            app: PRODUCT.to_owned(),
            version,
            runtime,
            transport,
            client,
            role,
            tree: None,
            serving: None,
            holding: None,
        }
    }

    /// Set the manifest fingerprint this peer is on.
    ///
    /// Separate from [`Self::new`] because the two are known at different
    /// times: identity at join, the tree only once a manifest has been fetched
    /// or scanned. Callers that never learn one leave it `None`.
    #[must_use]
    pub fn with_tree(mut self, tree: Option<String>) -> Self {
        self.tree = tree;
        self
    }

    /// Set which slots this peer can serve. See [`crate::serving`].
    #[must_use]
    pub fn with_serving(mut self, serving: Option<String>) -> Self {
        self.serving = serving;
        self
    }

    /// Set whether this peer holds any chunk at all. See [`Self::holding`].
    #[must_use]
    pub fn with_holding(mut self, holding: Option<bool>) -> Self {
        self.holding = holding;
        self
    }

    /// Whether this peer is worth dialling for bytes at all.
    ///
    /// The question every "is this a candidate source" filter is really asking,
    /// and the reason it is a method rather than a field test: [`Self::holding`]
    /// answers it exactly, but a peer built before that field existed omits it,
    /// and reading absent as *holds nothing* would make every older peer
    /// invisible to a newer one. So absent falls back to the older proxy —
    /// [`Self::serving`] being present — which is what the filters used before.
    ///
    /// Says nothing about *which* slots. A caller that needs a whole file must
    /// still consult `serving`; this only says the round trip is worth making.
    #[must_use]
    pub fn holds_something(&self) -> bool {
        self.holding.unwrap_or_else(|| self.serving.is_some())
    }

    /// JSON object for the `card` field of a meta merge
    /// (`{"peers":{nick:{"card": …}}}`).
    #[must_use]
    pub fn to_card_value(&self) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        map.insert(
            CARD_ENDPOINT.to_owned(),
            serde_json::Value::String(self.endpoint.clone()),
        );
        map.insert(
            CARD_APP.to_owned(),
            serde_json::Value::String(self.app.clone()),
        );
        map.insert(
            CARD_VERSION.to_owned(),
            serde_json::Value::String(self.version.clone()),
        );
        map.insert(
            CARD_RUNTIME.to_owned(),
            serde_json::Value::String(self.runtime.clone()),
        );
        map.insert(
            CARD_TRANSPORT.to_owned(),
            serde_json::Value::String(self.transport.clone()),
        );
        map.insert(
            CARD_CLIENT.to_owned(),
            serde_json::Value::String(self.client.clone()),
        );
        if let Some(role) = &self.role {
            map.insert(
                CARD_ROLE.to_owned(),
                serde_json::Value::String(role.clone()),
            );
        }
        // Omitted rather than written as null when unknown, so a peer that has
        // not learned its manifest yet is indistinguishable on the wire from
        // one built before the field existed. Both read as "cannot vouch".
        if let Some(tree) = &self.tree {
            map.insert(
                CARD_TREE.to_owned(),
                serde_json::Value::String(tree.clone()),
            );
        }
        if let Some(serving) = &self.serving {
            map.insert(
                CARD_SERVING.to_owned(),
                serde_json::Value::String(serving.clone()),
            );
        }
        if let Some(holding) = self.holding {
            map.insert(CARD_HOLDING.to_owned(), serde_json::Value::Bool(holding));
        }
        serde_json::Value::Object(map)
    }

    /// Parse a card object from meta JSON. Requires at least `endpoint` + `client`
    /// (or enough parts to rebuild `client`).
    #[must_use]
    pub fn from_card_value(value: &serde_json::Value) -> Option<Self> {
        let endpoint = value.get(CARD_ENDPOINT)?.as_str()?.to_owned();
        let app = value
            .get(CARD_APP)
            .and_then(serde_json::Value::as_str)
            .unwrap_or(PRODUCT)
            .to_owned();
        let version = value
            .get(CARD_VERSION)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_owned();
        let runtime = value
            .get(CARD_RUNTIME)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_owned();
        let transport = value
            .get(CARD_TRANSPORT)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_owned();
        let client = value
            .get(CARD_CLIENT)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .filter(|client| !client.is_empty())
            .unwrap_or_else(|| {
                if version.is_empty() || runtime.is_empty() || transport.is_empty() {
                    "unknown".to_owned()
                } else {
                    format_label(&version, &runtime, &transport)
                }
            });
        let role = value
            .get(CARD_ROLE)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        let tree = value
            .get(CARD_TREE)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        let serving = value
            .get(CARD_SERVING)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        let holding = value.get(CARD_HOLDING).and_then(serde_json::Value::as_bool);
        Some(Self {
            endpoint,
            app,
            version,
            runtime,
            transport,
            client,
            role,
            tree,
            serving,
            holding,
        })
    }
}

/// Build the display label: `agent-share v{version} ({runtime}, {transport})`.
#[must_use]
pub fn format_label(version: &str, runtime: &str, transport: &str) -> String {
    format!("{PRODUCT} v{version} ({runtime}, {transport})")
}

#[cfg(test)]
mod tests {
    use super::{CARD_TREE, PeerCard, format_label};

    fn card() -> PeerCard {
        PeerCard::new(
            "endpoint-a",
            "1.2.3",
            "rust",
            "unicast",
            Some("producer".to_owned()),
        )
    }

    #[test]
    fn a_tree_fingerprint_round_trips_through_the_card() {
        let with = card().with_tree(Some("0123456789abcdef".to_owned()));
        let parsed = PeerCard::from_card_value(&with.to_card_value()).expect("parse");
        assert_eq!(parsed, with);
        assert_eq!(parsed.tree.as_deref(), Some("0123456789abcdef"));
    }

    #[test]
    fn holding_round_trips_through_the_card() {
        for holding in [true, false] {
            let with = card().with_holding(Some(holding));
            let parsed = PeerCard::from_card_value(&with.to_card_value()).expect("parse");
            assert_eq!(parsed, with);
            assert_eq!(parsed.holding, Some(holding));
        }
    }

    /// **Absent must read as unknown, not as "holds nothing".**
    ///
    /// A peer built before `holding` existed omits the key, and every candidate
    /// filter asks [`PeerCard::holds_something`]. Reading absent as `false`
    /// would make each of those peers invisible to a newer one — a mesh that
    /// silently stops sharing across a version boundary.
    #[test]
    fn an_absent_holding_falls_back_to_serving() {
        let older = card().with_serving(Some("0-5".to_owned()));
        let parsed = PeerCard::from_card_value(&older.to_card_value()).expect("parse");
        assert_eq!(parsed.holding, None, "absent must not decode as false");
        assert!(parsed.holds_something(), "an older seeder must stay visible");

        let browsing = PeerCard::from_card_value(&card().to_card_value()).expect("parse");
        assert!(!browsing.holds_something(), "no serving and no flag is nothing");
    }

    /// The state the flag exists for: part-way through a transfer, nothing is
    /// servable whole and the peer is still worth asking for chunks.
    #[test]
    fn a_partial_holder_is_worth_dialling_with_no_serving() {
        let partial = card().with_serving(None).with_holding(Some(true));
        assert!(partial.holds_something());
    }

    /// Absent rather than null: a peer that has not learned its manifest yet
    /// simply does not carry the key.
    #[test]
    fn an_unknown_tree_is_omitted_from_the_wire() {
        let value = card().to_card_value();
        assert!(
            value.get(CARD_TREE).is_none(),
            "an unset tree must not be serialized at all, got {value}"
        );
    }

    /// A card missing fields must still yield what it can.
    ///
    /// Not a compatibility test — there are no older peers to be compatible
    /// with. This is robustness: cards ride an automerge document that peers
    /// merge concurrently, so an entry can be observed mid-write or arrive
    /// truncated. Losing a field must cost that field, never the peer, and
    /// never the roster it sits in.
    ///
    /// `endpoint` is the one exception, and deliberately: without it there is
    /// no identity to key the roster by, so the card is dropped whole.
    #[test]
    fn a_card_missing_fields_yields_what_it_can() {
        for absent in [CARD_TREE, "role", "version", "client"] {
            let mut value = card().with_tree(Some("dead".to_owned())).to_card_value();
            value
                .as_object_mut()
                .expect("card is an object")
                .remove(absent);
            let parsed = PeerCard::from_card_value(&value)
                .unwrap_or_else(|| panic!("a card missing {absent} must still parse"));
            assert_eq!(parsed.endpoint, "endpoint-a", "identity survives");
        }

        let mut headless = card().to_card_value();
        headless
            .as_object_mut()
            .expect("card is an object")
            .remove(super::CARD_ENDPOINT);
        assert!(
            PeerCard::from_card_value(&headless).is_none(),
            "without an endpoint there is nothing to key the roster by"
        );
    }

    #[test]
    fn label_matches_torrent_style_examples() {
        assert_eq!(
            format_label("1.2.3", "chrome", "webrtc"),
            "agent-share v1.2.3 (chrome, webrtc)"
        );
        assert_eq!(
            format_label("1.2.3", "rust", "unicast"),
            "agent-share v1.2.3 (rust, unicast)"
        );
    }

    #[test]
    fn card_round_trips() {
        let card = PeerCard::new("eid", "0.1.0", "chrome", "webrtc", Some("consumer".into()));
        let value = card.to_card_value();
        let parsed = PeerCard::from_card_value(&value).unwrap();
        assert_eq!(parsed, card);
        assert_eq!(parsed.client, "agent-share v0.1.0 (chrome, webrtc)");
    }
}
