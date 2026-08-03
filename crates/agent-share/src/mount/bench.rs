//! Synthetic throughput / latency bench.
//!
//! `agent-share bench --transport …` runs a producer that answers [`OP_BENCH`]
//! and encodes the chosen path in the ticket. `agent-share bench <ticket>`
//! runs the consumer using that path.

use std::time::{Duration, Instant};

use agent_share_proto::framing::{
    BENCH_ECHO_INTERVAL_SECS, BENCH_KIND_ECHO, BENCH_KIND_FILL, MAX_BENCH_ECHO_BYTES,
    MAX_BENCH_FILL_BYTES, decode_bench_request_prefix, decode_response_header,
    encode_bench_echo_request, encode_bench_fill_request,
};
use agent_share_proto::ticket::{TICKET_KIND_BENCH_RELAY, TICKET_KIND_BENCH_WEBRTC};
use anyhow::{Context, Result, bail};
use iroh::endpoint::{Connection, Incoming, RecvStream, SendStream};
use iroh::{Endpoint, EndpointAddr, EndpointId, SecretKey, TransportAddr};
use rand::RngCore;
use serde::Serialize;

use crate::lookup::{add_peer_addr, build_endpoint};
use crate::protocol::swarm::{LookupOpts, LookupSet, resolve_transfer_lookups};

use super::MountTicket;
use super::ReadStatus;
use super::{MOUNT_ALPN, OP_BENCH, REQUEST_HEADER_LEN, SECRET_LEN, WEBRTC_SIGNAL_ALPN};
use super::{dial_webrtc, serve_signal, wait_online};
use fofoca_iroh_webrtc_transport::{IceConfig, WebRtcHandle, WebRtcTransport};

/// Mount data path chosen by the bench producer (carried in the ticket kind).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BenchTransport {
    WebRtc,
    Relay,
}

impl BenchTransport {
    pub(crate) fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "webrtc" | "webrtc_only" | "webrtc-only" => Ok(Self::WebRtc),
            "relay" | "relay_only" | "relay-only" | "iroh_relay" | "iroh-relay" => Ok(Self::Relay),
            other => bail!("unknown transport {other:?}; expected webrtc or relay"),
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::WebRtc => "webrtc",
            Self::Relay => "relay",
        }
    }

    const fn ticket_kind(self) -> u8 {
        match self {
            Self::WebRtc => TICKET_KIND_BENCH_WEBRTC,
            Self::Relay => TICKET_KIND_BENCH_RELAY,
        }
    }

    fn from_ticket_kind(kind: u8) -> Result<Self> {
        match kind {
            TICKET_KIND_BENCH_WEBRTC => Ok(Self::WebRtc),
            TICKET_KIND_BENCH_RELAY => Ok(Self::Relay),
            other => bail!(
                "ticket has no bench transport (kind={other}); produce with --transport webrtc|relay"
            ),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct LatencyStats {
    pub min: f64,
    pub median: f64,
    pub p95: f64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct BenchReport {
    pub transport: String,
    pub connect_ms: f64,
    pub duration_s: f64,
    pub latency_ms: LatencyStats,
    pub throughput_mib_s: f64,
    pub bytes: u64,
    pub pings: u32,
}

/// Producer: mint a ticket for `transport`, serve `OP_BENCH` until interrupted.
pub(crate) async fn produce(transport: &str, json: bool) -> Result<()> {
    let transport = BenchTransport::parse(transport)?;
    let lookups = resolve_transfer_lookups(None, LookupSet::default())?;
    let (endpoint, ticket, secret, webrtc) = bind_bench(lookups, transport).await?;
    let command = format!("agent-share bench {}", ticket.encode());
    super::announce(
        json,
        &format!("bench ({} / synthetic OP_BENCH)", transport.as_str()),
        &command,
    );

    let local_id = endpoint.id();
    let ice = IceConfig::default();
    while let Some(incoming) = endpoint.accept().await {
        let webrtc = webrtc.clone();
        let ice = ice.clone();
        tokio::spawn(async move {
            if let Err(error) = accept_one(incoming, secret, local_id, webrtc.as_ref(), &ice).await
            {
                tracing::debug!(%error, "bench connection ended");
            }
        });
    }
    endpoint.close().await;
    Ok(())
}

async fn bind_bench(
    lookups: LookupOpts,
    transport: BenchTransport,
) -> Result<(
    Endpoint,
    MountTicket,
    [u8; SECRET_LEN],
    Option<WebRtcHandle>,
)> {
    let mut key_bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut key_bytes);
    let key = SecretKey::from_bytes(&key_bytes);

    let (endpoint, webrtc) = match transport {
        BenchTransport::Relay => {
            // clear_ip on *both* peers: dialing a relay-only addr is not
            // enough — iroh still upgrades to direct once both sides have IP
            // transports (see iroh `endpoint_two_relay_only_becomes_direct`).
            let endpoint = build_endpoint(
                &lookups,
                Some(key),
                None,
                vec![MOUNT_ALPN.to_vec()],
                None,
                true,
            )
            .await?;
            (endpoint, None)
        }
        BenchTransport::WebRtc => {
            let webrtc = WebRtcHandle::new(WebRtcTransport::new(key.public()));
            let endpoint = build_endpoint(
                &lookups,
                Some(key),
                None,
                vec![MOUNT_ALPN.to_vec(), WEBRTC_SIGNAL_ALPN.to_vec()],
                Some(webrtc.clone()),
                false,
            )
            .await?;
            debug_assert_eq!(endpoint.id(), webrtc.transport().local_id());
            (endpoint, Some(webrtc))
        }
    };

    if !lookups.is_loopback() {
        wait_online(&endpoint).await;
    }

    let mut secret = [0u8; SECRET_LEN];
    rand::rng().fill_bytes(&mut secret);
    let ticket = MountTicket {
        addr: endpoint.addr(),
        secret,
        lookups,
        kind: transport.ticket_kind(),
    };
    Ok((endpoint, ticket, secret, webrtc))
}

async fn accept_one(
    incoming: Incoming,
    secret: [u8; SECRET_LEN],
    local_id: EndpointId,
    webrtc: Option<&WebRtcHandle>,
    ice: &IceConfig,
) -> Result<()> {
    let conn = incoming.await.context("incoming connection failed")?;
    if conn.alpn() == WEBRTC_SIGNAL_ALPN {
        let Some(webrtc) = webrtc else {
            bail!("unexpected WebRTC signal on a relay-only bench producer");
        };
        return serve_signal(&conn, local_id, webrtc, ice).await;
    }
    serve_bench_connection(conn, secret).await
}

async fn serve_bench_connection(conn: Connection, secret: [u8; SECRET_LEN]) -> Result<()> {
    while let Ok((send, recv)) = conn.accept_bi().await {
        let conn = conn.clone();
        tokio::spawn(async move {
            if let Err(error) = serve_bench_stream(&conn, send, recv, &secret).await {
                tracing::debug!(%error, "bench stream ended");
            }
        });
    }
    Ok(())
}

async fn serve_bench_stream(
    conn: &Connection,
    mut send: SendStream,
    mut recv: RecvStream,
    secret: &[u8; SECRET_LEN],
) -> Result<()> {
    let mut header = [0u8; REQUEST_HEADER_LEN];
    if recv.read_exact(&mut header).await.is_err() {
        return Ok(());
    }
    if &header[..SECRET_LEN] != secret {
        conn.close(1u32.into(), b"bad secret");
        return Ok(());
    }
    if header[SECRET_LEN] != OP_BENCH {
        // Unknown / non-bench op: drop this stream only.
        return Ok(());
    }
    let mut prefix = [0u8; 5];
    if recv.read_exact(&mut prefix).await.is_err() {
        return Ok(());
    }
    let (kind, len) = decode_bench_request_prefix(&prefix)?;
    match kind {
        BENCH_KIND_ECHO => {
            if len == 0 || len > MAX_BENCH_ECHO_BYTES {
                return Ok(());
            }
            let mut payload = vec![0u8; len as usize];
            if recv.read_exact(&mut payload).await.is_err() {
                return Ok(());
            }
            send.write_all(&[ReadStatus::Ok.to_byte()]).await?;
            send.write_all(&len.to_le_bytes()).await?;
            send.write_all(&payload).await?;
        }
        BENCH_KIND_FILL => {
            if len == 0 || len > MAX_BENCH_FILL_BYTES {
                return Ok(());
            }
            send.write_all(&[ReadStatus::Ok.to_byte()]).await?;
            send.write_all(&len.to_le_bytes()).await?;
            // Patterned bytes so they compress poorly on the wire.
            let mut chunk = vec![0u8; 16 * 1024];
            for (index, byte) in chunk.iter_mut().enumerate() {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "index % 251 always fits in u8"
                )]
                {
                    *byte = (index % 251) as u8;
                }
            }
            let mut left = len as usize;
            while left > 0 {
                let take = left.min(chunk.len());
                send.write_all(&chunk[..take]).await?;
                left -= take;
            }
        }
        _ => return Ok(()),
    }
    let _ = send.finish();
    let _ = tokio::time::timeout(Duration::from_secs(2), send.stopped()).await;
    Ok(())
}

/// Consumer: connect using the transport encoded in the ticket flags.
pub(crate) async fn run(ticket: &str, duration_secs: u64, json: bool) -> Result<()> {
    let ticket = MountTicket::decode(ticket)?;
    let transport = BenchTransport::from_ticket_kind(ticket.kind)?;
    if !json {
        crate::util::output::status_out("Connecting", transport.as_str());
    }
    let connect_start = Instant::now();
    let (endpoint, conn, path) = connect_forced(&ticket, transport).await?;
    let connect_ms = connect_start.elapsed().as_secs_f64() * 1000.0;
    if !json {
        crate::util::output::status_out("Connected", &format!("{connect_ms:.1} ms ({path})"));
    }

    let duration = Duration::from_secs(duration_secs.max(1));
    if !json {
        crate::util::output::status_out("Benching", &format!("{}s", duration.as_secs()));
    }
    let measured = measure_window(&conn, &ticket.secret, duration, !json).await?;

    let report = BenchReport {
        transport: path.to_owned(),
        connect_ms,
        duration_s: measured.duration_s,
        latency_ms: measured.latency_ms,
        throughput_mib_s: measured.throughput_mib_s,
        bytes: measured.bytes,
        pings: measured.pings,
    };
    print_report(&report, json);
    conn.close(0u32.into(), b"bench done");
    endpoint.close().await;
    Ok(())
}

struct WindowStats {
    duration_s: f64,
    latency_ms: LatencyStats,
    throughput_mib_s: f64,
    bytes: u64,
    pings: u32,
}

async fn connect_forced(
    ticket: &MountTicket,
    transport: BenchTransport,
) -> Result<(Endpoint, Connection, &'static str)> {
    let mut key_bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut key_bytes);
    let key = SecretKey::from_bytes(&key_bytes);

    match transport {
        BenchTransport::Relay => {
            // Dial relay-only *and* clear IP transports locally so the path
            // cannot upgrade to direct after connect.
            let relay_only = relay_only_addr(&ticket.addr)?;
            let endpoint =
                build_endpoint(&ticket.lookups, Some(key), None, Vec::new(), None, true).await?;
            add_peer_addr(&endpoint, relay_only.clone())?;
            let conn = dial_with_retry(&endpoint, relay_only).await?;
            ensure_relay_selected(&conn).await?;
            Ok((endpoint, conn, "relay"))
        }
        BenchTransport::WebRtc => {
            // Two endpoints on one key — the same split the browser client
            // uses, and for the same reason. The signal endpoint keeps whatever
            // transports the ticket implies, because JSEP has to reach the
            // producer somehow (on a loopback ticket that is IP, since there is
            // no relay at all). The mount endpoint has IP cleared and the relay
            // disabled, so the only path it *can* select is the data channel.
            //
            // Clearing IP on a single endpoint cannot work: the same endpoint
            // has to dial the signal ALPN, and on loopback that would leave it
            // with no transport at all. Without the split the assertion below
            // fires with `paths=["*ip", "relay"]` — measured.
            let webrtc = WebRtcHandle::new(WebRtcTransport::new(key.public()));
            let signal_endpoint = build_endpoint(
                &ticket.lookups,
                Some(key.clone()),
                None,
                Vec::new(),
                None,
                false,
            )
            .await?;
            add_peer_addr(&signal_endpoint, ticket.addr.clone())?;

            let mut mount_lookups = ticket.lookups.clone();
            mount_lookups.relay = crate::protocol::swarm::RelayChoice::Disabled;
            let endpoint = build_endpoint(
                &mount_lookups,
                Some(key),
                None,
                Vec::new(),
                Some(webrtc.clone()),
                true,
            )
            .await?;

            let webrtc_only = Box::pin(dial_webrtc(
                &signal_endpoint,
                ticket.addr.clone(),
                &webrtc,
                &IceConfig::default(),
            ))
            .await
            .context("WebRTC signal/ICE failed")?;
            let conn = endpoint
                .connect(webrtc_only, MOUNT_ALPN)
                .await
                .context("dial mount over WebRTC")?;
            super::webrtc::ensure_webrtc_selected(&conn, "webrtc bench").await?;
            signal_endpoint.close().await;
            Ok((endpoint, conn, "webrtc"))
        }
    }
}

/// Wait briefly for path selection, then require the selected path to be relay.
async fn ensure_relay_selected(conn: &Connection) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        // `any` rather than `find(selected).is_some_and(relay)`: at most one
        // path is ever selected, so the two are equivalent, and this one does
        // not hand clippy a closure over a double reference.
        if conn
            .paths()
            .iter()
            .any(|path| path.is_selected() && path.is_relay())
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            let summary: Vec<String> = conn
                .paths()
                .iter()
                .map(|path| {
                    let kind = if path.is_relay() {
                        "relay"
                    } else if path.is_ip() {
                        "ip"
                    } else {
                        "other"
                    };
                    if path.is_selected() {
                        format!("*{kind}")
                    } else {
                        kind.to_owned()
                    }
                })
                .collect();
            bail!(
                "relay bench selected a non-relay path (paths={summary:?}); \
                 producer and consumer both need clear_ip_transports \
                 (restart producer with `bench --transport relay`)"
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Ticket addr with only `TransportAddr::Relay` entries — no IP, no custom.
fn relay_only_addr(addr: &EndpointAddr) -> Result<EndpointAddr> {
    let relays: Vec<TransportAddr> = addr
        .relay_urls()
        .cloned()
        .map(TransportAddr::Relay)
        .collect();
    if relays.is_empty() {
        bail!(
            "ticket has no iroh relay URL — cannot force relay transport \
             (producer must be reachable via a relay; loopback tickets cannot)"
        );
    }
    Ok(EndpointAddr::from_parts(addr.id, relays))
}

async fn dial_with_retry(endpoint: &Endpoint, addr: EndpointAddr) -> Result<Connection> {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        match endpoint.connect(addr.clone(), MOUNT_ALPN).await {
            Ok(conn) => return Ok(conn),
            Err(error) if Instant::now() < deadline => {
                tracing::warn!(%error, "bench dial failed; retrying");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Err(error) => return Err(error).context("could not reach bench producer"),
        }
    }
}

/// After connect: fill for throughput, echo once per interval for latency,
/// until `duration` elapses.
async fn measure_window(
    conn: &Connection,
    secret: &[u8; SECRET_LEN],
    duration: Duration,
    progress: bool,
) -> Result<WindowStats> {
    let start = Instant::now();
    let deadline = start + duration;
    let total_secs = duration.as_secs().max(1);
    let echo_every = Duration::from_secs(BENCH_ECHO_INTERVAL_SECS.max(1));
    let tick_every = Duration::from_secs(5);
    let mut next_echo = start;
    let mut next_tick = start + tick_every;
    let mut samples = Vec::new();
    let mut transferred = 0u64;

    while Instant::now() < deadline {
        let now = Instant::now();
        if progress && now >= next_tick {
            let elapsed = now.duration_since(start).as_secs().min(total_secs);
            crate::util::output::status_out("Benching", &format!("{elapsed}s / {total_secs}s"));
            next_tick = now + tick_every;
        }
        if Instant::now() >= next_echo {
            samples.push(echo_once(conn, secret).await?);
            next_echo = Instant::now() + echo_every;
        } else {
            transferred += fill_once(conn, secret, MAX_BENCH_FILL_BYTES).await?;
        }
    }

    // Guarantee at least one latency sample on very short windows.
    if samples.is_empty() {
        samples.push(echo_once(conn, secret).await?);
    }

    let duration_s = start.elapsed().as_secs_f64().max(1e-9);
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

async fn echo_once(conn: &Connection, secret: &[u8; SECRET_LEN]) -> Result<f64> {
    let payload = [0xABu8; 32];
    let request = encode_bench_echo_request(secret, &payload)?;
    let start = Instant::now();
    let (mut send, mut recv) = conn.open_bi().await.context("open echo stream")?;
    send.write_all(&request).await?;
    send.finish()?;
    let mut prefix = [0u8; 5];
    recv.read_exact(&mut prefix).await?;
    let len = decode_response_header(&prefix, MAX_BENCH_ECHO_BYTES)?;
    let mut body = vec![0u8; len as usize];
    recv.read_exact(&mut body).await?;
    if body != payload {
        bail!("echo payload mismatch");
    }
    Ok(start.elapsed().as_secs_f64() * 1000.0)
}

async fn fill_once(conn: &Connection, secret: &[u8; SECRET_LEN], want: u32) -> Result<u64> {
    let request = encode_bench_fill_request(secret, want)?;
    let (mut send, mut recv) = conn.open_bi().await.context("open fill stream")?;
    send.write_all(&request).await?;
    send.finish()?;
    let mut prefix = [0u8; 5];
    recv.read_exact(&mut prefix).await?;
    let len = decode_response_header(&prefix, want)?;
    let mut left = len as usize;
    let mut buf = vec![0u8; 64 * 1024];
    while left > 0 {
        let take = left.min(buf.len());
        recv.read_exact(&mut buf[..take]).await?;
        left -= take;
    }
    Ok(u64::from(len))
}

fn latency_stats(samples: &mut [f64]) -> LatencyStats {
    samples.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    let min = samples.first().copied().unwrap_or(0.0);
    let median = percentile(samples, 0.50);
    let p95 = percentile(samples, 0.95);
    LatencyStats { min, median, p95 }
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

fn print_report(report: &BenchReport, json: bool) {
    if json {
        match serde_json::to_string_pretty(report) {
            Ok(text) => println!("{text}"),
            Err(error) => tracing::error!(%error, "serialize bench report"),
        }
        return;
    }
    crate::util::output::status_out("Transport", &report.transport);
    crate::util::output::status_out("Connect", &format!("{:.1} ms", report.connect_ms));
    crate::util::output::status_out("Duration", &format!("{:.1} s", report.duration_s));
    crate::util::output::status_out(
        "Latency",
        &format!(
            "min {:.2} ms / median {:.2} ms / p95 {:.2} ms ({} pings)",
            report.latency_ms.min, report.latency_ms.median, report.latency_ms.p95, report.pings
        ),
    );
    crate::util::output::status_out(
        "Throughput",
        &format!(
            "{:.2} MiB/s over {} bytes",
            report.throughput_mib_s, report.bytes
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::swarm::LookupOpts;

    #[test]
    fn transport_parse_and_flags() {
        assert_eq!(
            BenchTransport::parse("webrtc").unwrap(),
            BenchTransport::WebRtc
        );
        assert_eq!(
            BenchTransport::parse("RELAY").unwrap(),
            BenchTransport::Relay
        );
        assert!(BenchTransport::parse("dynamic").is_err());
        assert_eq!(
            BenchTransport::from_ticket_kind(TICKET_KIND_BENCH_WEBRTC).unwrap(),
            BenchTransport::WebRtc
        );
        assert_eq!(
            BenchTransport::from_ticket_kind(TICKET_KIND_BENCH_RELAY).unwrap(),
            BenchTransport::Relay
        );
        assert!(BenchTransport::from_ticket_kind(0).is_err());
    }

    async fn spawn_loopback_producer(transport: BenchTransport) -> (Endpoint, MountTicket) {
        let (endpoint, ticket, secret, webrtc) = bind_bench(LookupOpts::loopback(), transport)
            .await
            .expect("bind");
        assert_eq!(ticket.kind, transport.ticket_kind());
        let local_id = endpoint.id();
        let ice = IceConfig::host_only();
        let accept = endpoint.clone();
        tokio::spawn(async move {
            while let Some(incoming) = accept.accept().await {
                let webrtc = webrtc.clone();
                let ice = ice.clone();
                tokio::spawn(async move {
                    let _ = accept_one(incoming, secret, local_id, webrtc.as_ref(), &ice).await;
                });
            }
        });
        (endpoint, ticket)
    }

    #[test]
    fn relay_only_addr_strips_ips() {
        let id = SecretKey::from_bytes(&[3u8; 32]).public();
        let full = EndpointAddr::from_parts(
            id,
            [
                TransportAddr::Ip("127.0.0.1:9".parse().unwrap()),
                TransportAddr::Relay("https://relay.example".parse().unwrap()),
            ],
        );
        let only = relay_only_addr(&full).unwrap();
        assert!(only.ip_addrs().next().is_none());
        assert_eq!(only.relay_urls().count(), 1);
        assert!(relay_only_addr(&EndpointAddr::new(id)).is_err());
    }

    /// A relay-forced dial refuses a ticket carrying no relay URL.
    ///
    /// The ticket is built by hand rather than by standing a producer up, and
    /// that is the fix rather than a shortcut. `spawn_loopback_producer` could
    /// not bind this case *at all*: `bind_bench` passes `clear_ip = true` for
    /// the relay transport — correct in production, where the lookups resolve a
    /// real relay — but against `LookupOpts::loopback()` the builder ends up
    /// with an empty transport list. `bind_addr` adds an IP transport,
    /// `RelayMode::Disabled` retains away every relay transport, no custom
    /// transport is registered, and `clear_ip_transports()` removes the IP one.
    /// iroh then fails the bind with "no valid address available" — its own
    /// `test_bind_addr_badport_notrequired_no_other_transports` asserts that
    /// exact string for that exact configuration.
    ///
    /// The test never needed the producer: `connect_forced` reads the ticket
    /// and rejects it in `relay_only_addr` before it binds or dials anything.
    /// Nothing here touches the network.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn relay_mode_rejects_loopback_ticket_without_relay_url() {
        let producer = SecretKey::from_bytes(&[9u8; 32]).public();
        let ticket = MountTicket {
            // Loopback: an IP path and no relay, which is what makes a
            // relay-forced dial impossible.
            addr: EndpointAddr::from_parts(
                producer,
                [TransportAddr::Ip("127.0.0.1:1".parse().unwrap())],
            ),
            secret: [0u8; SECRET_LEN],
            lookups: LookupOpts::loopback(),
            kind: TICKET_KIND_BENCH_RELAY,
        };

        let transport = BenchTransport::from_ticket_kind(ticket.kind).unwrap();
        let err = connect_forced(&ticket, transport)
            .await
            .expect_err("loopback has no relay URL");
        assert!(
            err.to_string().contains("no iroh relay URL"),
            "unexpected error: {err:#}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bench_over_webrtc() {
        let (endpoint, ticket) = spawn_loopback_producer(BenchTransport::WebRtc).await;
        let transport = BenchTransport::from_ticket_kind(ticket.kind).unwrap();
        let (_ep, conn, path) = connect_forced(&ticket, transport)
            .await
            .expect("connect webrtc");
        assert_eq!(path, "webrtc");
        let stats = measure_window(&conn, &ticket.secret, Duration::from_secs(1), false)
            .await
            .expect("window");
        assert!(stats.pings >= 1);
        assert!(stats.bytes > 0);
        assert!(stats.throughput_mib_s > 0.0);
        conn.close(0u32.into(), b"done");
        endpoint.close().await;
    }

    /// Stand up `sources` independent `WebRTC` bench producers, connect to all of
    /// them, and drive every connection concurrently for `secs`.
    ///
    /// Returns `(aggregate_mib_s, per_source_mib_s, median_rtt_ms)`.
    ///
    /// The RTT matters as much as the throughput: it is what proves the link
    /// emulator actually took effect. Without it a run where `pfctl` silently
    /// failed to apply is indistinguishable from one where added delay changed
    /// nothing.
    async fn measure_k_sources(sources: usize, secs: u64) -> (f64, Vec<f64>, f64) {
        let mut producers = Vec::new();
        for _ in 0..sources {
            producers.push(spawn_loopback_producer(BenchTransport::WebRtc).await);
        }

        // Connect to every producer *before* measuring, so ICE setup is not
        // inside the measurement window.
        let mut conns = Vec::new();
        for (_, ticket) in &producers {
            let transport = BenchTransport::from_ticket_kind(ticket.kind).unwrap();
            let (endpoint, conn, path) = connect_forced(ticket, transport).await.expect("connect");
            assert_eq!(
                path, "webrtc",
                "the SCTP ceiling claim is about WebRTC only"
            );
            conns.push((endpoint, conn, ticket.secret));
        }

        let mut set = tokio::task::JoinSet::new();
        for (_, conn, secret) in &conns {
            let conn = conn.clone();
            let secret = *secret;
            set.spawn(async move {
                let stats = measure_window(&conn, &secret, Duration::from_secs(secs), false)
                    .await
                    .expect("window");
                (stats.throughput_mib_s, stats.latency_ms.median)
            });
        }
        let mut per_source = Vec::new();
        let mut rtts = Vec::new();
        while let Some(res) = set.join_next().await {
            let (rate, rtt) = res.expect("bench task");
            per_source.push(rate);
            rtts.push(rtt);
        }
        rtts.sort_by(f64::total_cmp);
        let median_rtt = rtts.get(rtts.len() / 2).copied().unwrap_or(f64::NAN);

        for (endpoint, conn, _) in conns {
            conn.close(0u32.into(), b"done");
            endpoint.close().await;
        }
        for (endpoint, _) in producers {
            endpoint.close().await;
        }

        (per_source.iter().sum(), per_source, median_rtt)
    }

    /// **S0.4 — the decisive experiment.**
    ///
    /// RFC 01 justifies swarming partly on throughput: the `WebRTC` data
    /// channel ceiling is `SCTP`'s 128 `KiB` receive window, one source cannot
    /// be tuned around it, and a second source is therefore worth ~2×. If that
    /// holds, aggregate throughput scales with the number of sources. If
    /// aggregate is flat, the ceiling is somewhere shared (CPU, loopback, the
    /// `SCTP` stack itself) and RFC 01's throughput motivation collapses to
    /// resilience and fan-out only.
    ///
    /// Measurement, not assertion — it prints a table and only asserts that
    /// the run produced numbers. Loopback has no RTT, so this is the *most
    /// favourable* case for a shared-ceiling result; a delayed-link run
    /// (`dnctl`/`pfctl` at 50 ms) is the follow-up, not a substitute.
    ///
    /// Expensive (K `WebRTC` producers, full ICE each), so it is `#[ignore]`d
    /// like `mount::real_mount_round_trip`. Run it by hand:
    /// `cargo test -p agent-share --lib s04_ -- --ignored --nocapture`
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    #[ignore = "measurement, not a regression guard; takes ~2min"]
    async fn s04_multi_source_throughput_scaling() {
        let secs: u64 = std::env::var("S04_SECS")
            .ok()
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(5);
        let reps: usize = std::env::var("S04_REPS")
            .ok()
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(3);
        let source_counts = [1usize, 2, 4];

        // Repeat and interleave: a single pass through K=1,2,4 confounds the
        // scaling question with order and warm-up. If repeats of the same K
        // disagree with each other as much as different Ks do, the harness is
        // not measuring link capacity and no scaling claim can be read off it.
        let mut samples: std::collections::BTreeMap<usize, Vec<f64>> =
            std::collections::BTreeMap::new();
        let mut rtts: Vec<f64> = Vec::new();
        for _ in 0..reps {
            for count in source_counts {
                let (aggregate, _, rtt) = measure_k_sources(count, secs).await;
                samples.entry(count).or_default().push(aggregate);
                rtts.push(rtt);
            }
        }
        rtts.sort_by(f64::total_cmp);
        let rtt_ms = rtts[rtts.len() / 2];

        println!("\nS0.4 — aggregate throughput vs source count (WebRTC, loopback)");
        println!("       {reps} reps x {secs}s per K, interleaved\n");
        println!("   K       min      median         max      spread");
        let mut medians = std::collections::BTreeMap::new();
        for (count, values) in &samples {
            let mut sorted = values.clone();
            sorted.sort_by(f64::total_cmp);
            let (min, max) = (sorted[0], sorted[sorted.len() - 1]);
            let median = sorted[sorted.len() / 2];
            medians.insert(*count, median);
            println!(
                "  {count:>2}   {min:>7.1}   {median:>9.1}   {max:>9.1}   {:>7.2}x",
                if min > 0.0 { max / min } else { f64::NAN }
            );
        }

        let base = medians[&1];
        println!("\n  scaling vs K=1 (median):");
        for (count, median) in &medians {
            println!("    K={count}: {:.2}x", median / base);
        }
        // A per-connection window W bounds one connection to W/RTT. Printing
        // the implied window makes the mechanism falsifiable instead of
        // rhetorical: if the measured K=1 rate matches 128 KiB/RTT, RFC 01's
        // stated cause is right; if it implies megabytes, the binding window is
        // QUIC's, not SCTP's.
        let implied_window_kib = base * 1024.0 * (rtt_ms / 1000.0);
        println!("\n  measured median RTT: {rtt_ms:.1} ms");
        println!(
            "  K=1 rate {base:.1} MiB/s at that RTT implies a per-connection window of \
             ~{implied_window_kib:.0} KiB"
        );
        if rtt_ms < 5.0 {
            println!(
                "\n  CAVEAT: RTT is ~0, so no window is binding and this run cannot\n  \
                 test the per-connection-ceiling claim. Re-run under a link\n  \
                 emulator (dnctl/pfctl dummynet on lo0)."
            );
        }
        println!(
            "\n  CAVEAT: all producers and consumers share one host and one CPU,\n  \
             so CPU contention is a confound at higher K regardless of RTT.\n  \
             Note also that this transport negotiates the data channel\n  \
             **unreliable and unordered** (`MaxRetransmits {{ retransmits: 0 }}`,\n  \
             fofoca-iroh-webrtc-transport host/jsep.rs), so the classic reliable-\n  \
             SCTP receive-window stall RFC 01 cites is not the mechanism here.\n"
        );

        for values in samples.values() {
            assert!(
                values.iter().all(|rate| *rate > 0.0),
                "every run must move bytes"
            );
        }
    }
}
