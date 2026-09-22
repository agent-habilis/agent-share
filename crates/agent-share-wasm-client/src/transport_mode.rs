//! How `ShareClient::connect` carries mount data.

/// Preference for the mount data path.
///
/// The relay brokers the connection and never carries file bytes, so both
/// variants end on the WebRTC data channel. They differ in what else may
/// answer: [`Self::Dynamic`] also races a seeder in the mesh.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TransportMode {
    /// The origin's data channel only. ICE failure is fatal.
    WebRtc,
    /// The origin's data channel, raced against a seeder in the mesh.
    #[default]
    Dynamic,
}

impl TransportMode {
    /// Parse a mode string. Empty / omitted maps to [`Self::Dynamic`].
    ///
    /// Accepts `webrtc` and `dynamic` (case-insensitive), and a few
    /// aliases (`webrtc_only`, `preferred`, `webrtc_preferred`).
    ///
    /// # Errors
    /// Unknown non-empty value.
    pub fn parse(raw: Option<&str>) -> Result<Self, String> {
        let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
            return Ok(Self::Dynamic);
        };
        match raw.to_ascii_lowercase().as_str() {
            "webrtc" | "webrtc_only" | "webrtc-only" => Ok(Self::WebRtc),
            "dynamic" | "preferred" | "webrtc_preferred" | "webrtc-preferred" => Ok(Self::Dynamic),
            other => Err(format!(
                "unknown transport mode {other:?}; expected webrtc or dynamic"
            )),
        }
    }

    /// Requested mode as a stable lowercase label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WebRtc => "webrtc",
            Self::Dynamic => "dynamic",
        }
    }
}

#[cfg(test)]
mod tests {
    // These run on wasm32, the only target this crate builds for — see the
    // dev-dependency note in `Cargo.toml`. Renaming the attribute keeps the
    // tests written as ordinary `#[test]` functions, so nothing below has to
    // know which harness it is under.
    use wasm_bindgen_test::wasm_bindgen_test as test;

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
            TransportMode::parse(Some("webrtc_preferred")).unwrap(),
            TransportMode::Dynamic
        );
        assert_eq!(
            TransportMode::parse(Some("preferred")).unwrap(),
            TransportMode::Dynamic
        );
    }

    /// The relay is a rendezvous, never a data path: `relay` stops being a mode.
    #[test]
    fn rejects_relay() {
        let err = TransportMode::parse(Some("relay")).unwrap_err();
        assert!(err.contains("unknown transport mode"), "{err}");
        assert!(TransportMode::parse(Some("relay-only")).is_err());
        assert!(TransportMode::parse(Some("iroh_relay")).is_err());
    }

    #[test]
    fn rejects_unknown() {
        let err = TransportMode::parse(Some("turn")).unwrap_err();
        assert!(err.contains("unknown transport mode"), "{err}");
        assert!(err.contains("webrtc"), "{err}");
    }
}
