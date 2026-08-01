//! How `ShareClient::connect` carries mount data.

/// Preference for the mount data path.
///
/// Default is [`Self::Dynamic`]: both WebRTC and the iroh relay are available,
/// WebRTC is tried first, and a failed ICE/channel falls back to relay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TransportMode {
    /// WebRTC only. ICE failure is fatal.
    WebRtc,
    /// Iroh relay / ticket address only. Skip WebRTC.
    Relay,
    /// Both on: WebRTC preferred, then iroh relay fallback.
    #[default]
    Dynamic,
}

impl TransportMode {
    /// Parse a mode string. Empty / omitted maps to [`Self::Dynamic`].
    ///
    /// Accepts `webrtc`, `relay`, `dynamic` (case-insensitive), and a few
    /// aliases (`webrtc_only`, `relay_only`, `preferred`, `webrtc_preferred`).
    ///
    /// # Errors
    /// Unknown non-empty value.
    pub fn parse(raw: Option<&str>) -> Result<Self, String> {
        let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
            return Ok(Self::Dynamic);
        };
        match raw.to_ascii_lowercase().as_str() {
            "webrtc" | "webrtc_only" | "webrtc-only" => Ok(Self::WebRtc),
            "relay" | "relay_only" | "relay-only" | "iroh_relay" | "iroh-relay" => Ok(Self::Relay),
            "dynamic" | "preferred" | "webrtc_preferred" | "webrtc-preferred" => Ok(Self::Dynamic),
            other => Err(format!(
                "unknown transport mode {other:?}; expected webrtc, relay, or dynamic"
            )),
        }
    }

    /// Wire label for the path that actually carried mount bytes.
    #[must_use]
    pub const fn selected_label(self, used_webrtc: bool) -> &'static str {
        if used_webrtc { "webrtc" } else { "relay" }
    }
}

#[cfg(test)]
mod tests {
    use super::TransportMode;

    #[test]
    fn omitted_and_empty_default_to_dynamic() {
        assert_eq!(TransportMode::parse(None).unwrap(), TransportMode::Dynamic);
        assert_eq!(
            TransportMode::parse(Some("")).unwrap(),
            TransportMode::Dynamic
        );
        assert_eq!(
            TransportMode::parse(Some("  ")).unwrap(),
            TransportMode::Dynamic
        );
        assert_eq!(TransportMode::default(), TransportMode::Dynamic);
    }

    #[test]
    fn bench_rejects_dynamic_at_call_site() {
        // ShareClient::bench maps omitted/dynamic to an error; parse itself
        // still accepts dynamic for ordinary connect.
        assert_eq!(
            TransportMode::parse(Some("dynamic")).unwrap(),
            TransportMode::Dynamic
        );
        assert_ne!(
            TransportMode::parse(Some("webrtc")).unwrap(),
            TransportMode::Dynamic
        );
        assert_ne!(
            TransportMode::parse(Some("relay")).unwrap(),
            TransportMode::Dynamic
        );
    }

    #[test]
    fn parses_canonical_names_case_insensitively() {
        assert_eq!(
            TransportMode::parse(Some("webrtc")).unwrap(),
            TransportMode::WebRtc
        );
        assert_eq!(
            TransportMode::parse(Some("WEBRTC")).unwrap(),
            TransportMode::WebRtc
        );
        assert_eq!(
            TransportMode::parse(Some("relay")).unwrap(),
            TransportMode::Relay
        );
        assert_eq!(
            TransportMode::parse(Some("Dynamic")).unwrap(),
            TransportMode::Dynamic
        );
    }

    #[test]
    fn parses_aliases() {
        assert_eq!(
            TransportMode::parse(Some("webrtc_only")).unwrap(),
            TransportMode::WebRtc
        );
        assert_eq!(
            TransportMode::parse(Some("relay-only")).unwrap(),
            TransportMode::Relay
        );
        assert_eq!(
            TransportMode::parse(Some("webrtc_preferred")).unwrap(),
            TransportMode::Dynamic
        );
        assert_eq!(
            TransportMode::parse(Some("preferred")).unwrap(),
            TransportMode::Dynamic
        );
        assert_eq!(
            TransportMode::parse(Some("iroh_relay")).unwrap(),
            TransportMode::Relay
        );
    }

    #[test]
    fn rejects_unknown() {
        let err = TransportMode::parse(Some("turn")).unwrap_err();
        assert!(err.contains("unknown transport mode"), "{err}");
        assert!(err.contains("webrtc"), "{err}");
    }

    #[test]
    fn selected_label_reports_path_not_mode() {
        assert_eq!(TransportMode::Dynamic.selected_label(true), "webrtc");
        assert_eq!(TransportMode::Dynamic.selected_label(false), "relay");
        assert_eq!(TransportMode::WebRtc.selected_label(true), "webrtc");
        assert_eq!(TransportMode::Relay.selected_label(false), "relay");
    }
}
