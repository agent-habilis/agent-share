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
        }
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
        Some(Self {
            endpoint,
            app,
            version,
            runtime,
            transport,
            client,
            role,
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
    use super::{PeerCard, format_label};

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
