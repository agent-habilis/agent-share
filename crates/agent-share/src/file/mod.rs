//! Shared file-transfer helpers vendored from agent-habilis/swarm's
//! `src/file/mod.rs` — trimmed to the slice the mount feature uses
//! (`wait_online`, `human_bytes`, and the `walk`/`wire` submodules).

use std::time::Duration;

use iroh::Endpoint;

pub(crate) mod walk;
pub(crate) mod wire;

/// Best-effort wait (≤5s) for the endpoint to publish reachable addresses, so a
/// freshly-printed ticket resolves immediately. Never blocks forever.
pub(crate) async fn wait_online(endpoint: &Endpoint) {
    let _ = tokio::time::timeout(Duration::from_secs(5), endpoint.online()).await;
}

/// Format a byte count for humans (`512B`, `1.5KB`, `3.4MB`).
pub(crate) fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes}B");
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "human-readable display only, not used for any calculation"
    )]
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1}{}", UNITS[unit])
}
