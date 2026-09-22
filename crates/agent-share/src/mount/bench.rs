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
use agent_share_proto::ticket::{TICKET_KIND_BENCH_QUIC, TICKET_KIND_BENCH_WEBRTC};
use anyhow::{Context, Result, bail};
use fofoca::iroh::endpoint::{Connection, Incoming, RecvStream, SendStream};
use fofoca::iroh::{Endpoint, EndpointAddr, EndpointId, SecretKey};
use fofoca_iroh_webrtc_transport::{IceConfig, WebRtcHandle, WebRtcTransport};
use rand::RngCore;
use serde::Serialize;

use super::MountTicket;
use super::ReadStatus;
use super::{MOUNT_ALPN, OP_BENCH, REQUEST_HEADER_LEN, SECRET_LEN, WEBRTC_SIGNAL_ALPN};
use super::{dial_webrtc, serve_signal, wait_online};
use crate::lookup::{add_peer_addr, build_endpoint};
use crate::protocol::swarm::{LookupOpts, LookupSet, resolve_transfer_lookups};

/// Mount data path chosen by the bench producer (carried in the ticket kind).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BenchTransport {
    WebRtc,
    /// Plain iroh QUIC over UDP: no `WebRTC` wrapper.
    ///
    /// The control leg. Without it the only synthetic numbers available are
    /// wrapped ones, and the wrapper's cost cannot be separated from the
    /// request shape without going through NFS as well.
    Quic,
}

impl BenchTransport {
    pub(crate) fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "webrtc" | "webrtc_only" | "webrtc-only" => Ok(Self::WebRtc),
            "quic" | "direct" | "udp" => Ok(Self::Quic),
            other => bail!("unknown transport {other:?}; expected webrtc or quic"),
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::WebRtc => "webrtc",
            Self::Quic => "quic",
        }
    }

    const fn ticket_kind(self) -> u8 {
        match self {
            Self::WebRtc => TICKET_KIND_BENCH_WEBRTC,
            Self::Quic => TICKET_KIND_BENCH_QUIC,
        }
    }

    fn from_ticket_kind(kind: u8) -> Result<Self> {
        match kind {
            TICKET_KIND_BENCH_WEBRTC => Ok(Self::WebRtc),
            TICKET_KIND_BENCH_QUIC => Ok(Self::Quic),
            other => bail!(
                "ticket has no bench transport (kind={other}); produce with --transport webrtc|quic"
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
    /// Fill requests kept in flight. `1` is the historical serial behaviour.
    pub depth: usize,
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
        BenchTransport::Quic => {
            // Nothing forced and nothing cleared: whatever iroh would pick for
            // an ordinary share. On a loopback swarm that is a direct IP path.
            let endpoint = build_endpoint(
                &lookups,
                Some(key),
                None,
                vec![MOUNT_ALPN.to_vec()],
                None,
                false,
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
        // A bench share is synthetic — no directory, no bytes, nothing worth
        // protecting — so it never carries a password.
        flags: 0,
        mesh_id: None,
        author: None,
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
            bail!("unexpected WebRTC signal on a QUIC bench producer");
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
pub(crate) async fn run(ticket: &str, duration_secs: u64, depth: usize, json: bool) -> Result<()> {
    if depth == 0 {
        bail!("--depth must be at least 1");
    }
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
    let measured = measure_window(&conn, &ticket.secret, duration, !json, depth).await?;

    let report = BenchReport {
        transport: path.to_owned(),
        depth,
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
        BenchTransport::Quic => {
            // The control leg, so it forces nothing — no `ensure_*_selected`
            // gate, because "whatever iroh picks" is exactly the path an
            // ordinary mount would take.
            let endpoint =
                build_endpoint(&ticket.lookups, Some(key), None, Vec::new(), None, false).await?;
            add_peer_addr(&endpoint, ticket.addr.clone())?;
            let conn = dial_with_retry(&endpoint, ticket.addr.clone()).await?;
            Ok((endpoint, conn, "quic"))
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
    depth: usize,
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
    let mut inflight: tokio::task::JoinSet<Result<u64>> = tokio::task::JoinSet::new();

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
            continue;
        }
        if depth == 1 {
            // Kept inline rather than routed through the JoinSet so the
            // depth-1 number stays exactly the shape the committed baseline
            // measured — no task spawn between the request and the wire.
            transferred += fill_once(conn, secret, MAX_BENCH_FILL_BYTES).await?;
            continue;
        }
        while inflight.len() < depth {
            let conn_task = conn.clone();
            let secret_task = *secret;
            inflight.spawn(async move {
                fill_once(&conn_task, &secret_task, MAX_BENCH_FILL_BYTES).await
            });
        }
        if let Some(joined) = inflight.join_next().await {
            transferred += joined.context("bench fill task panicked")??;
        }
    }

    // Drain rather than abort: `duration_s` is read from the clock *after*
    // this, so the bytes and the seconds describe the same window either way,
    // and aborting would silently discard up to `depth` MiB of real transfer.
    while let Some(joined) = inflight.join_next().await {
        transferred += joined.context("bench fill task panicked")??;
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
        assert!(BenchTransport::parse("dynamic").is_err());
        assert_eq!(
            BenchTransport::from_ticket_kind(TICKET_KIND_BENCH_WEBRTC).unwrap(),
            BenchTransport::WebRtc
        );
        assert!(BenchTransport::from_ticket_kind(0).is_err());
    }

    /// The relay is a rendezvous, never a data path — not even for the bench.
    #[test]
    fn relay_is_not_a_bench_transport() {
        let err = BenchTransport::parse("relay").expect_err("relay is not a data path");
        assert!(
            err.to_string().contains("unknown transport"),
            "unexpected error: {err:#}"
        );
        assert!(BenchTransport::parse("relay-only").is_err());
        assert!(BenchTransport::parse("iroh_relay").is_err());
        // Kind 2 stays reserved: an old relay-bench ticket no longer decodes.
        assert!(BenchTransport::from_ticket_kind(2).is_err());
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bench_over_webrtc() {
        let (endpoint, ticket) = spawn_loopback_producer(BenchTransport::WebRtc).await;
        let transport = BenchTransport::from_ticket_kind(ticket.kind).unwrap();
        let (_ep, conn, path) = connect_forced(&ticket, transport)
            .await
            .expect("connect webrtc");
        assert_eq!(path, "webrtc");
        let stats = measure_window(&conn, &ticket.secret, Duration::from_secs(1), false, 1)
            .await
            .expect("window");
        assert!(stats.pings >= 1);
        assert!(stats.bytes > 0);
        assert!(stats.throughput_mib_s > 0.0);
        conn.close(0u32.into(), b"done");
        endpoint.close().await;
    }
}
