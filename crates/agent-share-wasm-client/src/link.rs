//! One meter for both lanes.
//!
//! A share's bytes ride either a `WebRTC` data channel or an iroh relay, and
//! nothing above this module should have to know which. Two sources answer, and
//! they are not interchangeable:
//!
//! - The QUIC state machine, via [`Connection::stats`]. Synchronous, and
//!   identical on every lane, because it counts *above* the transport split —
//!   the `WebRTC` lane is an iroh custom transport and the relay lane is iroh's
//!   own, but both carry the same connection. This is the source that makes a
//!   relay-only tab report at all.
//! - `getStats` on a `WebRTC` candidate pair. Async, per peer, and the only
//!   source for peers we hold no [`Connection`] to — mesh peers — but silent on
//!   the relay path, where there is no `RTCPeerConnection` to ask.
//!
//! They are different **units**, and adding them together would be nonsense:
//! QUIC bytes sit below DTLS/SCTP on the `WebRTC` lane and below the relay's
//! `WebSocket` framing on the other, so a QUIC total is always smaller than the
//! candidate-pair total for the very same traffic.
//!
//! # Scope
//!
//! [`read_quic`] answers for **one connection** — in practice the mount
//! connection, which is where every share byte flows. Mesh and gossip traffic
//! rides separate connections held in `agent-habilis-mesh`'s `UnicastPool`,
//! which is `pub(crate)` there and unreachable from here. For a *share* readout
//! that is the right boundary, but it does mean these numbers are share traffic,
//! not everything the tab does.

use std::collections::HashMap;
use std::time::Duration;

use fofoca::iroh::endpoint::Connection;
use fofoca::iroh::{EndpointId, TransportAddr};
use fofoca_iroh_webrtc_transport::BrowserHubTransport;

/// Cumulative counters differenced into rates.
///
/// `f64` throughout because that is what `getStats` hands back, and the QUIC
/// side converts on the way in — the counters sit well under 2^53, so nothing is
/// lost and taking `u64` here would only invent precision the other source
/// cannot supply.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Meter {
    pub(crate) sent: f64,
    pub(crate) received: f64,
    /// Bytes per second since the previous sample. Zero until there are two.
    pub(crate) up_bps: f64,
    pub(crate) down_bps: f64,
    /// Round-trip time in milliseconds, when the path has been measured.
    pub(crate) rtt_ms: Option<f64>,
    /// `js_sys::Date::now()` of this sample, for the next difference.
    pub(crate) at_ms: f64,
}

impl Meter {
    /// Fold a fresh reading in, carrying rates over from `self`.
    ///
    /// Rates come from differencing cumulative counters — neither source
    /// exposes instantaneous throughput. A non-advancing clock or a counter that
    /// went backwards (a renegotiated pair, a replaced path) yields *no* rate
    /// rather than a negative or infinite one.
    pub(crate) fn sample(self, sent: f64, received: f64, rtt_ms: Option<f64>, now_ms: f64) -> Self {
        let elapsed_s = (now_ms - self.at_ms) / 1000.0;
        let rate = |current: f64, previous: f64| {
            if self.at_ms > 0.0 && elapsed_s > 0.0 && current >= previous {
                (current - previous) / elapsed_s
            } else {
                0.0
            }
        };
        Self {
            up_bps: rate(sent, self.sent),
            down_bps: rate(received, self.received),
            sent,
            received,
            rtt_ms,
            at_ms: now_ms,
        }
    }

    /// The shape the UI reads: totals *and* the rates derived from them.
    pub(crate) fn to_json(self) -> serde_json::Value {
        serde_json::json!({
            "sent": self.sent,
            "received": self.received,
            "up_bps": self.up_bps,
            "down_bps": self.down_bps,
            "rtt_ms": self.rtt_ms,
        })
    }
}

/// A lane's meter, plus the one thing about a lane that is not a counter.
///
/// `selected` lives here rather than only in the sampler's return value because
/// two callers render this cache — the sampler and `info` — and a field only
/// one of them knew about is exactly how they drifted apart the first time.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct LaneMeter {
    pub(crate) meter: Meter,
    /// Whether this path was carrying application data at the last sample.
    pub(crate) selected: bool,
}

impl LaneMeter {
    fn to_json(self, label: &str) -> serde_json::Value {
        let mut row = self.meter.to_json();
        row["label"] = serde_json::Value::String(label.to_owned());
        row["selected"] = serde_json::Value::Bool(self.selected);
        row
    }
}

/// Render a lane cache for JS: `{ total, lanes: [...] }`.
///
/// The single renderer for both [`crate::ShareClient::sample_link`] (which folds
/// a fresh reading in first) and `info` (which must not). Sharing it is what
/// keeps a sampled reading and a reported one the same shape.
pub(crate) fn to_json(cache: &HashMap<String, LaneMeter>, total_key: &str) -> serde_json::Value {
    let lanes: Vec<serde_json::Value> = cache
        .iter()
        .filter(|(label, _)| label.as_str() != total_key)
        .map(|(label, lane)| lane.to_json(label))
        .collect();
    serde_json::json!({
        "total": cache.get(total_key).copied().unwrap_or_default().meter.to_json(),
        "lanes": lanes,
    })
}

/// One network path of a connection, read from the QUIC state machine.
#[derive(Debug, Clone)]
pub(crate) struct Lane {
    /// `relay` / `ip` / `webrtc` — see [`path_label`].
    pub(crate) label: String,
    /// Whether this is the path carrying application data right now.
    pub(crate) selected: bool,
    pub(crate) sent: u64,
    pub(crate) received: u64,
    pub(crate) rtt_ms: Option<f64>,
}

/// One path's transport, as the label the UI and the mode assertion both use.
///
/// `data_path`, `mount_paths` and the `WebRTC`-mode check have to agree on what
/// counts as "webrtc", so they share this rather than each carrying a copy of
/// the match.
pub(crate) fn path_label(addr: &TransportAddr) -> String {
    match addr {
        TransportAddr::Relay(_) => "relay".to_owned(),
        TransportAddr::Ip(_) => "ip".to_owned(),
        TransportAddr::Custom(custom)
            if custom.id() == fofoca_iroh_webrtc_transport::WEBRTC_TRANSPORT_ID =>
        {
            "webrtc".to_owned()
        }
        other => format!("{other:?}"),
    }
}

/// An unmeasured path reports `Duration::ZERO`, which is not a round trip.
///
/// Rendering that as `0.0 ms` would claim a perfect link on a path nobody has
/// timed yet, so it becomes `None` and the UI shows a dash.
fn rtt_ms(rtt: Duration) -> Option<f64> {
    (!rtt.is_zero()).then_some(rtt.as_secs_f64() * 1000.0)
}

/// Every lane of `connection`, with its wire bytes. Synchronous.
///
/// One `paths()` walk. The same iterator `selected_path_label` and `path_labels`
/// already use — those ask each path for its address, this one also asks for its
/// counters.
pub(crate) fn read_quic(connection: &Connection) -> Vec<Lane> {
    connection
        .paths()
        .iter()
        .map(|path| {
            let stats = path.stats();
            Lane {
                label: path_label(path.remote_addr()),
                selected: path.is_selected(),
                sent: stats.udp_tx.bytes,
                received: stats.udp_rx.bytes,
                rtt_ms: rtt_ms(stats.rtt),
            }
        })
        .collect()
}

/// The connection's totals: `(sent, received)` wire bytes across every path.
///
/// Not a sum of [`read_quic`] — `ConnectionStats` also carries paths that have
/// since closed, so a connection that migrated lanes keeps the bytes it moved on
/// the old one instead of silently dropping them.
pub(crate) fn read_quic_total(connection: &Connection) -> (u64, u64) {
    let stats = connection.stats();
    (stats.udp_tx.bytes, stats.udp_rx.bytes)
}

/// One peer's `WebRTC` candidate-pair counters: `(sent, received, rtt_ms)`.
///
/// `None` when the peer has no live session in this hub — a gossip-only peer has
/// no candidate pair, and `0/0` there would read as "nothing sent" rather than
/// "not measured".
pub(crate) async fn read_ice(
    hub: &BrowserHubTransport,
    remote: &EndpointId,
) -> Option<(f64, f64, Option<f64>)> {
    hub.selected_pair_stats(remote)
        .await
        .map(|(sent, received, rtt)| (sent, received, rtt.map(|seconds| seconds * 1000.0)))
}
