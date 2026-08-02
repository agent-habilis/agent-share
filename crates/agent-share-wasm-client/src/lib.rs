//! The browser client: read a share over WebRTC and/or the iroh relay.
//!
//! No web-specific protocol. This speaks the same `agent-share/mount/1` ALPN
//! the CLI does, over the same `fofoca-iroh-webrtc-transport`, using the same
//! `agent-share-proto` wire types — the browser is a peer, not a special case.
//!
//! # Transport modes
//!
//! [`ShareClient::connect`] takes an optional mode (`webrtc` | `relay` |
//! `dynamic`). Omit it for the default: **both on, WebRTC preferred** — try a
//! data channel first, then fall back to the iroh relay (ticket ladder, often
//! `relay.agent-habilis.com`) when ICE fails. Force `webrtc` or `relay` in
//! tests.
//!
//! # The two-connection dance (WebRTC path)
//!
//! iroh only fans a connect's Initial out to candidate paths **while the
//! remote has no selected path**, so a live connection cannot be upgraded onto
//! a newly attached transport. Hence:
//!
//! 1. Dial `agent-share/webrtc-signal/1` over the relay and swap one JSEP
//!    envelope each way.
//! 2. Open a **fresh** connection to `agent-share/mount/1` against an address
//!    carrying only the `WebRTC` custom addr.
//!
//! When host/mDNS and NAT hairpin both fail, ICE can still connect through a
//! short-lived public TURN server in `IceServers`. Under `dynamic`, a failed
//! ICE then uses the iroh relay for mount bytes.

use std::sync::Arc;

use agent_share_proto::framing::{
    self, BENCH_ECHO_INTERVAL_SECS, DEFAULT_BENCH_DURATION_SECS, MAX_BENCH_ECHO_BYTES,
    MAX_BENCH_FILL_BYTES, MAX_MANIFEST_BYTES, MOUNT_ALPN, SECRET_LEN, WEBRTC_SIGNAL_ALPN,
};
use agent_share_proto::manifest::{ManifestDelta, MountManifest};
use agent_share_proto::ticket::{MountTicket, TICKET_FLAG_BENCH_RELAY, TICKET_FLAG_BENCH_WEBRTC};
use iroh::endpoint::{Connection, presets};
use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey, TransportAddr};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

mod live_state;
mod produce;
mod transport_mode;

pub use transport_mode::TransportMode;

use fofoca_iroh_webrtc_transport::{
    BrowserHubTransport, BrowserSession, IceServers, MAX_ENVELOPE_BYTES, SignalEnvelope,
    WebRtcHandle, browser_offer, custom_addr, log_signal_sdps,
};

/// A connected share, ready to list and read.
#[wasm_bindgen]
pub struct ShareClient {
    connection: Connection,
    secret: [u8; SECRET_LEN],
    /// `"webrtc"` or `"relay"` — the path that actually carries mount bytes.
    data_path: String,
    // Held so the hub (and its data channel) outlives the connection when used.
    _hub: Option<Arc<BrowserHubTransport>>,
    _session: Option<BrowserSession>,
    _endpoint: Endpoint,
}

#[wasm_bindgen]
impl ShareClient {
    /// Decode `ticket` and open the mount connection.
    ///
    /// `transport` is `webrtc`, `relay`, or `dynamic` (case-insensitive).
    /// Omit it for **dynamic**: both paths on, WebRTC preferred, iroh relay
    /// fallback. See [`TransportMode`].
    ///
    /// # Errors
    /// The ticket is malformed, the mode is unknown, the producer is
    /// unreachable, or (in `webrtc` mode) ICE fails with no fallback.
    #[wasm_bindgen]
    pub async fn connect(
        ticket: String,
        transport: Option<String>,
    ) -> Result<ShareClient, JsValue> {
        console_error_panic_hook::set_once();
        let mode = TransportMode::parse(transport.as_deref())
            .map_err(|message| JsValue::from_str(&message))?;
        let ticket = MountTicket::decode(&ticket).map_err(|error| err("decode ticket", &error))?;
        match mode {
            TransportMode::Relay => connect_relay(ticket).await,
            TransportMode::WebRtc => connect_webrtc(ticket, /*allow_relay_fallback=*/ false).await,
            TransportMode::Dynamic => {
                connect_webrtc(ticket, /*allow_relay_fallback=*/ true).await
            }
        }
    }

    /// Which path carries mount data: `"webrtc"` or `"relay"`.
    #[must_use]
    #[wasm_bindgen(getter)]
    pub fn transport(&self) -> String {
        self.data_path.clone()
    }

    /// The whole tree, in one shot: `{ dirs: [...], files: [...] }`.
    ///
    /// One request by design — the protocol has no per-directory listing op,
    /// so navigation is instant and only *bytes* are lazy.
    ///
    /// # Errors
    /// The producer refuses the request or the manifest does not decode.
    pub async fn manifest(&self) -> Result<JsValue, JsValue> {
        let (mut send, mut recv) = self
            .connection
            .open_bi()
            .await
            .map_err(|error| err("open manifest stream", &error))?;
        send.write_all(&framing::encode_manifest_request(&self.secret))
            .await
            .map_err(|error| err("send manifest request", &error))?;
        send.finish().map_err(|error| err("finish", &error))?;

        let len = read_header(&mut recv, MAX_MANIFEST_BYTES).await?;
        let mut bytes = vec![0u8; len as usize];
        recv.read_exact(&mut bytes)
            .await
            .map_err(|error| err("read manifest", &error))?;
        let manifest =
            MountManifest::decode(&bytes).map_err(|error| err("decode manifest", &error))?;
        serde_wasm(&manifest)
    }

    /// Follow the share as it changes, calling `on_manifest` with the whole
    /// tree every time it does.
    ///
    /// The callback receives a complete manifest rather than a difference:
    /// deltas are how the *wire* stays small, but a UI wants the current
    /// state, and reassembling it here means the browser cannot drift from
    /// what the producer thinks it sent. Fire-and-forget — it runs until the
    /// connection ends, and a producer too old for the op simply never calls
    /// back.
    ///
    /// # Errors
    /// Never: the subscription runs in the background. Stream failures retry
    /// every 3s while the connection is alive; a clean zero-frame end means
    /// the producer does not support live watch and the loop stops.
    pub async fn watch(&self, on_manifest: js_sys::Function) -> Result<(), JsValue> {
        let conn = self.connection.clone();
        let secret = self.secret;
        wasm_bindgen_futures::spawn_local(async move {
            loop {
                match follow_watch(&conn, &secret, &on_manifest).await {
                    WatchEnd::Unsupported => return,
                    WatchEnd::Retryable => {
                        if conn.close_reason().is_some() {
                            return;
                        }
                        wait_ms(3_000).await;
                    }
                }
            }
        });
        Ok(())
    }

    /// One byte range of one file, addressed by its index in the manifest.
    ///
    /// Capped at `MAX_READ_LEN` (256 KiB) per call by the protocol; chunk
    /// larger reads yourself.
    ///
    /// # Errors
    /// A bad index, an unreadable file, or a length over the producer's cap.
    pub async fn read(&self, index: u32, offset: u64, len: u32) -> Result<Vec<u8>, JsValue> {
        let (mut send, mut recv) = self
            .connection
            .open_bi()
            .await
            .map_err(|error| err("open read stream", &error))?;
        send.write_all(&framing::encode_read_request(
            &self.secret,
            index,
            offset,
            len,
        ))
        .await
        .map_err(|error| err("send read request", &error))?;
        send.finish().map_err(|error| err("finish", &error))?;

        let got = read_header(&mut recv, len).await?;
        let mut data = vec![0u8; got as usize];
        recv.read_exact(&mut data)
            .await
            .map_err(|error| err("read body", &error))?;
        Ok(data)
    }

    /// Connect using the transport encoded in the ticket and measure for
    /// `duration_secs` (default 30s after connect).
    ///
    /// `on_status` is called with a status object
    /// `{ stage: "connecting"|"connected"|"benching"|"progress", … }` when set.
    ///
    /// # Errors
    /// Ticket lacks a bench transport flag, connect failure, or producer
    /// lacks `OP_BENCH`.
    pub async fn bench(
        ticket: String,
        duration_secs: Option<u64>,
        on_status: Option<js_sys::Function>,
    ) -> Result<JsValue, JsValue> {
        console_error_panic_hook::set_once();
        let ticket = MountTicket::decode(&ticket).map_err(|error| err("decode ticket", &error))?;
        let transport_label = match ticket.flags {
            TICKET_FLAG_BENCH_RELAY => "relay",
            TICKET_FLAG_BENCH_WEBRTC => "webrtc",
            other => {
                return Err(JsValue::from_str(&format!(
                    "ticket has no bench transport (flags={other}); produce with --transport webrtc|relay"
                )));
            }
        };
        emit_status(
            on_status.as_ref(),
            &serde_json::json!({ "stage": "connecting", "transport": transport_label }),
        );

        let connect_start = now_ms();
        let client = match ticket.flags {
            // Bench relay must not fall through to direct IP (same-machine
            // benches were reporting ~localhost numbers labeled "relay").
            TICKET_FLAG_BENCH_RELAY => connect_relay_only(ticket).await?,
            TICKET_FLAG_BENCH_WEBRTC => {
                connect_webrtc(ticket, /*allow_relay_fallback=*/ false).await?
            }
            _ => unreachable!("validated above"),
        };
        let connect_ms = now_ms() - connect_start;
        emit_status(
            on_status.as_ref(),
            &serde_json::json!({
                "stage": "connected",
                "transport": client.data_path,
                "connect_ms": connect_ms,
            }),
        );

        let duration_secs = duration_secs.unwrap_or(DEFAULT_BENCH_DURATION_SECS).max(1);
        let duration_ms = (duration_secs as f64) * 1000.0;
        emit_status(
            on_status.as_ref(),
            &serde_json::json!({ "stage": "benching", "duration_s": duration_secs }),
        );
        let measured = measure_window(&client, duration_ms, on_status.as_ref()).await?;

        let report = BenchReport {
            transport: client.data_path.clone(),
            connect_ms,
            duration_s: measured.duration_s,
            latency_ms: measured.latency_ms,
            throughput_mib_s: measured.throughput_mib_s,
            bytes: measured.bytes,
            pings: measured.pings,
        };
        client.connection.close(0u32.into(), b"bench done");
        client._endpoint.close().await;
        serde_wasm(&report)
    }
}

fn emit_status(on_status: Option<&js_sys::Function>, value: &serde_json::Value) {
    let Some(callback) = on_status else {
        return;
    };
    let Ok(js) = serde_wasm(value) else {
        return;
    };
    let _ = callback.call1(&JsValue::NULL, &js);
}

#[derive(serde::Serialize)]
struct LatencyStats {
    min: f64,
    median: f64,
    p95: f64,
}

#[derive(serde::Serialize)]
struct BenchReport {
    transport: String,
    connect_ms: f64,
    duration_s: f64,
    latency_ms: LatencyStats,
    throughput_mib_s: f64,
    bytes: u64,
    pings: u32,
}

struct WindowStats {
    duration_s: f64,
    latency_ms: LatencyStats,
    throughput_mib_s: f64,
    bytes: u64,
    pings: u32,
}

fn now_ms() -> f64 {
    js_sys::Date::now()
}

async fn measure_window(
    client: &ShareClient,
    duration_ms: f64,
    on_status: Option<&js_sys::Function>,
) -> Result<WindowStats, JsValue> {
    let start = now_ms();
    let deadline = start + duration_ms;
    let total_secs = (duration_ms / 1000.0).round().max(1.0) as u64;
    let echo_every_ms = (BENCH_ECHO_INTERVAL_SECS.max(1) as f64) * 1000.0;
    let tick_every_ms = 5_000.0;
    let mut next_echo = start;
    let mut next_tick = start + tick_every_ms;
    let mut samples = Vec::new();
    let mut transferred = 0u64;

    while now_ms() < deadline {
        let now = now_ms();
        if on_status.is_some() && now >= next_tick {
            let elapsed = ((now - start) / 1000.0).floor().max(0.0) as u64;
            let elapsed = elapsed.min(total_secs);
            emit_status(
                on_status,
                &serde_json::json!({
                    "stage": "progress",
                    "elapsed_s": elapsed,
                    "duration_s": total_secs,
                }),
            );
            next_tick = now + tick_every_ms;
        }
        if now_ms() >= next_echo {
            samples.push(echo_once(client).await?);
            next_echo = now_ms() + echo_every_ms;
        } else {
            transferred += fill_once(client, MAX_BENCH_FILL_BYTES).await?;
        }
    }
    if samples.is_empty() {
        samples.push(echo_once(client).await?);
    }

    let duration_s = ((now_ms() - start) / 1000.0).max(1e-9);
    let pings = u32::try_from(samples.len()).unwrap_or(u32::MAX);
    #[expect(
        clippy::cast_precision_loss,
        reason = "throughput display only; byte counts stay exact in `bytes`"
    )]
    let mib_s = (transferred as f64) / (1024.0 * 1024.0) / duration_s;
    Ok(WindowStats {
        duration_s,
        latency_ms: latency_stats(&mut samples),
        throughput_mib_s: mib_s,
        bytes: transferred,
        pings,
    })
}

async fn echo_once(client: &ShareClient) -> Result<f64, JsValue> {
    let payload = [0xABu8; 32];
    let request = framing::encode_bench_echo_request(&client.secret, &payload)
        .map_err(|error| err("encode echo", &error))?;
    let start = now_ms();
    let (mut send, mut recv) = client
        .connection
        .open_bi()
        .await
        .map_err(|error| err("open echo stream", &error))?;
    send.write_all(&request)
        .await
        .map_err(|error| err("send echo", &error))?;
    send.finish().map_err(|error| err("finish echo", &error))?;
    let len = read_header(&mut recv, MAX_BENCH_ECHO_BYTES).await?;
    let mut body = vec![0u8; len as usize];
    recv.read_exact(&mut body)
        .await
        .map_err(|error| err("read echo", &error))?;
    if body != payload {
        return Err(JsValue::from_str("echo payload mismatch"));
    }
    Ok(now_ms() - start)
}

async fn fill_once(client: &ShareClient, want: u32) -> Result<u64, JsValue> {
    let request = framing::encode_bench_fill_request(&client.secret, want)
        .map_err(|error| err("encode fill", &error))?;
    let (mut send, mut recv) = client
        .connection
        .open_bi()
        .await
        .map_err(|error| err("open fill stream", &error))?;
    send.write_all(&request)
        .await
        .map_err(|error| err("send fill", &error))?;
    send.finish().map_err(|error| err("finish fill", &error))?;
    let len = read_header(&mut recv, want).await?;
    let mut left = len as usize;
    let mut buf = vec![0u8; 64 * 1024];
    while left > 0 {
        let take = left.min(buf.len());
        recv.read_exact(&mut buf[..take])
            .await
            .map_err(|error| err("read fill", &error))?;
        left -= take;
    }
    Ok(u64::from(len))
}

fn latency_stats(samples: &mut [f64]) -> LatencyStats {
    samples.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    let min = samples.first().copied().unwrap_or(0.0);
    LatencyStats {
        min,
        median: percentile(samples, 0.50),
        p95: percentile(samples, 0.95),
    }
}

fn percentile(sorted: &[f64], fraction: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "index into a small sample vec; fraction is in 0..=1"
    )]
    let idx = ((sorted.len() as f64 - 1.0) * fraction).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// Dial mount over the ticket address (IP and/or iroh relay). No WebRTC.
async fn connect_relay(ticket: MountTicket) -> Result<ShareClient, JsValue> {
    ensure_reachable_addr(&ticket.addr)?;
    let key = SecretKey::generate();
    let endpoint = Endpoint::builder(presets::Minimal)
        .secret_key(key)
        .relay_mode(relay_mode(&ticket))
        .bind()
        .await
        .map_err(|error| err("bind relay endpoint", &error))?;

    let connection = endpoint
        .connect(ticket.addr.clone(), MOUNT_ALPN)
        .await
        .map_err(|error| err("dial the mount ALPN over iroh relay/IP", &error))?;

    Ok(ShareClient {
        connection,
        secret: ticket.secret,
        data_path: "relay".to_owned(),
        _hub: None,
        _session: None,
        _endpoint: endpoint,
    })
}

/// Dial mount using **only** the ticket's relay URL(s) — no direct IP.
async fn connect_relay_only(ticket: MountTicket) -> Result<ShareClient, JsValue> {
    let relays: Vec<TransportAddr> = ticket
        .addr
        .relay_urls()
        .cloned()
        .map(TransportAddr::Relay)
        .collect();
    if relays.is_empty() {
        return Err(JsValue::from_str(
            "ticket has no iroh relay URL — cannot force relay transport",
        ));
    }
    let relay_only = EndpointAddr::from_parts(ticket.addr.id, relays);
    let key = SecretKey::generate();
    // Browser endpoints have no IP transports (`clear_ip_transports` is
    // host-only); dialing relay-only plus path assertion is enough here.
    let endpoint = Endpoint::builder(presets::Minimal)
        .secret_key(key)
        .relay_mode(relay_mode(&ticket))
        .bind()
        .await
        .map_err(|error| err("bind relay-only endpoint", &error))?;

    let connection = dial_with_retry(&endpoint, relay_only).await?;
    ensure_relay_selected(&connection).await?;

    Ok(ShareClient {
        connection,
        secret: ticket.secret,
        data_path: "relay".to_owned(),
        _hub: None,
        _session: None,
        _endpoint: endpoint,
    })
}

/// Retry dial for up to 90s (same policy as the native bench consumer).
async fn dial_with_retry(
    endpoint: &Endpoint,
    addr: EndpointAddr,
) -> Result<Connection, JsValue> {
    let deadline = now_ms() + 90_000.0;
    loop {
        match endpoint.connect(addr.clone(), MOUNT_ALPN).await {
            Ok(conn) => return Ok(conn),
            Err(error) if now_ms() < deadline => {
                web_sys::console::warn_1(&JsValue::from_str(&format!(
                    "bench dial failed; retrying: {error}"
                )));
                wait_ms(2_000).await;
            }
            Err(error) => {
                return Err(err(
                    "could not reach bench producer over iroh relay",
                    &error,
                ));
            }
        }
    }
}

/// Wait briefly for path selection, then require the selected path to be relay.
async fn ensure_relay_selected(conn: &Connection) -> Result<(), JsValue> {
    let deadline = now_ms() + 5_000.0;
    loop {
        if conn
            .paths()
            .iter()
            .find(|p| p.is_selected())
            .is_some_and(|p| p.is_relay())
        {
            return Ok(());
        }
        if now_ms() >= deadline {
            let summary: Vec<String> = conn
                .paths()
                .iter()
                .map(|p| {
                    let kind = if p.is_relay() {
                        "relay"
                    } else if p.is_ip() {
                        "ip"
                    } else {
                        "other"
                    };
                    if p.is_selected() {
                        format!("*{kind}")
                    } else {
                        kind.to_owned()
                    }
                })
                .collect();
            return Err(JsValue::from_str(&format!(
                "relay bench selected a non-relay path (paths={summary:?}); \
                 producer and consumer both need clear_ip_transports \
                 (restart producer with `bench --transport relay`)"
            )));
        }
        wait_ms(50).await;
    }
}

enum WatchEnd {
    /// Clean stream end before any frame — producer does not do live watch.
    Unsupported,
    /// Mid-stream error; retry on the same connection if it is still open.
    Retryable,
}

/// One OP_WATCH attempt: open a bi-stream, send the request, apply frames.
async fn follow_watch(
    conn: &Connection,
    secret: &[u8; SECRET_LEN],
    on_manifest: &js_sys::Function,
) -> WatchEnd {
    let Ok((mut send, mut recv)) = conn.open_bi().await else {
        return WatchEnd::Retryable;
    };
    if send
        .write_all(&framing::encode_watch_request(secret))
        .await
        .is_err()
    {
        return WatchEnd::Retryable;
    }
    if send.finish().is_err() {
        return WatchEnd::Retryable;
    }

    let mut manifest = MountManifest::default();
    let mut saw_frame = false;
    loop {
        let Ok(len) = read_header(&mut recv, MAX_MANIFEST_BYTES).await else {
            return if saw_frame {
                WatchEnd::Retryable
            } else {
                WatchEnd::Unsupported
            };
        };
        let mut body = vec![0u8; len as usize];
        if recv.read_exact(&mut body).await.is_err() {
            return WatchEnd::Retryable;
        }
        let Some((kind, payload)) = body.split_first() else {
            return WatchEnd::Retryable;
        };
        let applied = match *kind {
            framing::WATCH_FRAME_MANIFEST => {
                MountManifest::decode(payload).map(|fresh| manifest = fresh)
            }
            framing::WATCH_FRAME_DELTA => {
                ManifestDelta::decode(payload).map(|delta| manifest.apply(&delta))
            }
            _ => return WatchEnd::Retryable,
        };
        if applied.is_err() {
            return WatchEnd::Retryable;
        }
        saw_frame = true;
        let Ok(value) = serde_wasm(&manifest) else {
            return WatchEnd::Retryable;
        };
        if on_manifest.call1(&JsValue::NULL, &value).is_err() {
            web_sys::console::warn_1(&JsValue::from_str(
                "[share] watch callback threw; continuing subscription",
            ));
            continue;
        }
    }
}

async fn wait_ms(millis: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        if let Some(window) = web_sys::window() {
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, millis);
        }
    });
    let _ = JsFuture::from(promise).await;
}

/// Signal + WebRTC mount dial; optionally fall back to iroh relay/IP.
async fn connect_webrtc(
    ticket: MountTicket,
    allow_relay_fallback: bool,
) -> Result<ShareClient, JsValue> {
    let producer = ticket.addr.id;
    ensure_reachable_addr(&ticket.addr)?;

    // One key, two endpoints. A custom transport can only be registered at
    // build time, and the transport itself does not exist until the JSEP
    // exchange has produced a session — which needs an endpoint to happen
    // over. Keep the signaller alive until WebRTC succeeds (or fallback dials)
    // so `dynamic` can reuse it for the iroh path.
    let key = SecretKey::generate();
    let local = key.public();
    let hub = BrowserHubTransport::new(local);

    let signaller = Endpoint::builder(presets::Minimal)
        .secret_key(key.clone())
        .relay_mode(relay_mode(&ticket))
        .bind()
        .await
        .map_err(|error| err("bind signalling endpoint", &error))?;

    let session = match negotiate(&signaller, ticket.addr.clone(), local, &hub).await {
        Ok(session) => session,
        Err(error) if allow_relay_fallback => {
            web_sys::console::warn_1(&JsValue::from_str(&format!(
                "[agent-share] WebRTC signal/ICE failed ({error:?}); falling back to iroh relay/IP"
            )));
            return finish_relay_fallback(signaller, ticket).await;
        }
        Err(error) => {
            signaller.close().await;
            return Err(error);
        }
    };

    let handle = WebRtcHandle::new(Arc::clone(&hub));
    let data_endpoint = Endpoint::builder(presets::Minimal)
        .secret_key(key)
        .relay_mode(RelayMode::Disabled)
        .add_custom_transport(handle.transport())
        .bind()
        .await
        .map_err(|error| err("bind data endpoint", &error))?;

    let webrtc_only =
        EndpointAddr::from_parts(producer, [TransportAddr::Custom(custom_addr(producer))]);
    match data_endpoint.connect(webrtc_only, MOUNT_ALPN).await {
        Ok(connection) => {
            signaller.close().await;
            Ok(ShareClient {
                connection,
                secret: ticket.secret,
                data_path: "webrtc".to_owned(),
                _hub: Some(hub),
                _session: Some(session),
                _endpoint: data_endpoint,
            })
        }
        Err(error) if allow_relay_fallback => {
            web_sys::console::warn_1(&JsValue::from_str(&format!(
                "[agent-share] WebRTC mount dial failed ({error}); falling back to iroh relay/IP"
            )));
            data_endpoint.close().await;
            // Session/hub drop with data_endpoint; signaller still has relay.
            finish_relay_fallback(signaller, ticket).await
        }
        Err(error) => {
            data_endpoint.close().await;
            signaller.close().await;
            Err(err("dial the mount ALPN over WebRTC", &error))
        }
    }
}

async fn finish_relay_fallback(
    endpoint: Endpoint,
    ticket: MountTicket,
) -> Result<ShareClient, JsValue> {
    let connection = endpoint
        .connect(ticket.addr.clone(), MOUNT_ALPN)
        .await
        .map_err(|error| err("dial the mount ALPN over iroh relay/IP (fallback)", &error))?;
    Ok(ShareClient {
        connection,
        secret: ticket.secret,
        data_path: "relay".to_owned(),
        _hub: None,
        _session: None,
        _endpoint: endpoint,
    })
}

fn ensure_reachable_addr(addr: &EndpointAddr) -> Result<(), JsValue> {
    if addr.relay_urls().next().is_none() && addr.ip_addrs().next().is_none() {
        return Err(JsValue::from_str(
            "ticket has no relay or IP address — the producer was not reachable when the ticket was minted",
        ));
    }
    Ok(())
}

/// Swap one JSEP envelope each way over the signal ALPN, then attach into `hub`.
async fn negotiate(
    endpoint: &Endpoint,
    producer: EndpointAddr,
    local: iroh_base::EndpointId,
    hub: &BrowserHubTransport,
) -> Result<BrowserSession, JsValue> {
    let producer_id = producer.id;
    let conn = endpoint
        .connect(producer, WEBRTC_SIGNAL_ALPN)
        .await
        .map_err(|error| err("dial the signal ALPN", &error))?;
    let (mut send, mut recv) = conn
        .open_bi()
        .await
        .map_err(|error| err("open signal stream", &error))?;

    let ice = IceServers::with_turn_fallback().await;
    let (pending, offer) = browser_offer(local, &ice)
        .await
        .map_err(|error| js_stage("build offer", error))?;
    let encoded = serde_json::to_vec(&offer).map_err(|error| err("encode offer", &error))?;
    send.write_all(&encoded)
        .await
        .map_err(|error| err("send offer", &error))?;
    send.finish().map_err(|error| err("finish", &error))?;

    let raw = recv
        .read_to_end(MAX_ENVELOPE_BYTES)
        .await
        .map_err(|error| {
            err(
                "read answer (producer never answered — is it still sharing?)",
                &error,
            )
        })?;
    let answer: SignalEnvelope =
        serde_json::from_slice(&raw).map_err(|error| err("parse answer", &error))?;
    let session = match pending.complete(hub, &answer).await {
        Ok(session) => session,
        Err(error) => {
            log_signal_sdps("consumer", &offer, &answer);
            return Err(js_stage("complete WebRTC offer", error));
        }
    };
    // Hub is keyed by the answer's claimed id; the mount dial uses the ticket
    // id. A mismatch would blackhole transmits with no useful error.
    if session.remote != producer_id {
        return Err(JsValue::from_str(&format!(
            "answer endpoint id {} does not match ticket producer {}",
            session.remote, producer_id
        )));
    }

    conn.close(0u32.into(), b"jsep done");
    Ok(session)
}

fn js_stage(context: &str, error: JsValue) -> JsValue {
    JsValue::from_str(&format!("{context}: {error:?}"))
}

/// Read the `status(1) ‖ len(u32)` prefix every response carries.
async fn read_header(recv: &mut iroh::endpoint::RecvStream, cap: u32) -> Result<u32, JsValue> {
    let mut prefix = [0u8; 5];
    recv.read_exact(&mut prefix)
        .await
        .map_err(|error| err("read response header", &error))?;
    framing::decode_response_header(&prefix, cap).map_err(|error| err("response", &error))
}

/// The relay ladder the ticket carries. `Disabled` on a loopback ticket, where
/// there is nothing to reach.
fn relay_mode(ticket: &MountTicket) -> RelayMode {
    use agent_share_proto::lookup::RelayChoice;
    match &ticket.lookups.relay {
        RelayChoice::Disabled => RelayMode::Disabled,
        RelayChoice::Pinned => iroh::endpoint::default_relay_mode(),
        RelayChoice::Custom(ladder) => RelayMode::custom(ladder.iter().cloned()),
    }
}

fn err(context: &str, error: &impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&format!("{context}: {error}"))
}

fn serde_wasm<T: serde::Serialize>(value: &T) -> Result<JsValue, JsValue> {
    let json = serde_json::to_string(value).map_err(|error| err("serialize", &error))?;
    js_sys::JSON::parse(&json)
}
