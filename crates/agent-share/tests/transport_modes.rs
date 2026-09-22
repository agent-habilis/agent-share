//! Native analogs of the wasm `ShareClient` transport modes.
//!
//! The browser client lives behind `web-sys`, so these tests exercise the same
//! three strategies on host endpoints: `relay` (ticket addr only), `webrtc`
//! (data channel only), and `dynamic` (`WebRTC` preferred, fall back to direct
//! when the data-channel path cannot be used).

use std::sync::Arc;
use std::time::Duration;

use agent_share_proto::framing::{MOUNT_ALPN, SECRET_LEN, WEBRTC_SIGNAL_ALPN};
use agent_share_proto::manifest::MountManifest;
use fofoca::iroh::endpoint::presets;
use fofoca::iroh::{Endpoint, EndpointAddr, SecretKey, TransportAddr};
use fofoca_iroh_webrtc_transport::{
    IceConfig, MAX_ENVELOPE_BYTES, SignalEnvelope, WebRtcHandle, WebRtcTransport, answer_with,
    custom_addr, offer_with,
};

fn ice() -> IceConfig {
    IceConfig::host_only()
}

fn temp_tree() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "agent-share-transport-modes-{}",
        rand::RngCore::next_u64(&mut rand::rng())
    ));
    std::fs::create_dir_all(&dir).expect("create fixture dir");
    std::fs::write(dir.join("hello.txt"), b"hello world").expect("write hello");
    dir
}

async fn bind_plain(alpns: Vec<Vec<u8>>) -> Endpoint {
    let mut key_bytes = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rng(), &mut key_bytes);
    Endpoint::builder(presets::Minimal)
        .secret_key(SecretKey::from_bytes(&key_bytes))
        .relay_mode(fofoca::iroh::RelayMode::Disabled)
        .clear_address_lookup()
        .alpns(alpns)
        .bind()
        .await
        .expect("bind plain endpoint")
}

async fn bind_webrtc(alpns: Vec<Vec<u8>>) -> (Endpoint, WebRtcHandle) {
    let mut key_bytes = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rng(), &mut key_bytes);
    let key = SecretKey::from_bytes(&key_bytes);
    let handle = WebRtcHandle::new(WebRtcTransport::new(key.public()));
    let endpoint = Endpoint::builder(presets::Minimal)
        .secret_key(key)
        .relay_mode(fofoca::iroh::RelayMode::Disabled)
        .clear_address_lookup()
        .alpns(alpns)
        .add_custom_transport(handle.transport())
        .bind()
        .await
        .expect("bind webrtc endpoint");
    (endpoint, handle)
}

fn spawn_mount_server(
    endpoint: Endpoint,
    secret: [u8; SECRET_LEN],
    tree: std::path::PathBuf,
) -> tokio::task::JoinHandle<()> {
    let (manifest, paths) = agent_share::test_support::scan(&tree).expect("scan");
    let shared_tree = agent_share::test_support::live_tree(tree, manifest, paths);
    // These fixtures serve unprotected shares, so the token is the secret.
    let auth = agent_share_proto::auth::ShareAuth::new(&secret, None);
    tokio::spawn(async move {
        while let Some(incoming) = endpoint.accept().await {
            let shared_tree = Arc::clone(&shared_tree);
            tokio::spawn(async move {
                let Ok(conn) = incoming.await else { return };
                if conn.alpn() == MOUNT_ALPN {
                    let _ =
                        agent_share::test_support::serve_mount(conn, auth, shared_tree, None).await;
                }
            });
        }
    })
}

fn spawn_signal_and_mount_server(
    endpoint: Endpoint,
    webrtc: WebRtcHandle,
    producer_id: fofoca::iroh::EndpointId,
    secret: [u8; SECRET_LEN],
    tree: std::path::PathBuf,
) -> tokio::task::JoinHandle<()> {
    let (manifest, paths) = agent_share::test_support::scan(&tree).expect("scan");
    let shared_tree = agent_share::test_support::live_tree(tree, manifest, paths);
    // These fixtures serve unprotected shares, so the token is the secret.
    let auth = agent_share_proto::auth::ShareAuth::new(&secret, None);
    tokio::spawn(async move {
        while let Some(incoming) = endpoint.accept().await {
            let shared_tree = Arc::clone(&shared_tree);
            let webrtc = webrtc.clone();
            tokio::spawn(async move {
                let Ok(conn) = incoming.await else { return };
                if conn.alpn() == WEBRTC_SIGNAL_ALPN {
                    let (mut send, mut recv) = conn.accept_bi().await.expect("accept signal");
                    let raw = recv
                        .read_to_end(MAX_ENVELOPE_BYTES)
                        .await
                        .expect("read offer");
                    let offer: SignalEnvelope = serde_json::from_slice(&raw).expect("parse offer");
                    let (pending, answer) = answer_with(producer_id, &offer, &ice())
                        .await
                        .expect("build answer");
                    send.write_all(&serde_json::to_vec(&answer).expect("encode answer"))
                        .await
                        .expect("send answer");
                    send.finish().expect("finish");
                    let session = Box::pin(pending.complete(Duration::from_secs(20)))
                        .await
                        .expect("complete answer");
                    webrtc.attach(conn.remote_id(), session).expect("attach");
                } else if conn.alpn() == MOUNT_ALPN {
                    let _ =
                        agent_share::test_support::serve_mount(conn, auth, shared_tree, None).await;
                }
            });
        }
    })
}

async fn fetch_manifest(
    conn: &fofoca::iroh::endpoint::Connection,
    secret: &[u8; SECRET_LEN],
) -> MountManifest {
    let (mut send, mut recv) = conn.open_bi().await.expect("open manifest stream");
    send.write_all(&agent_share_proto::framing::encode_manifest_request(secret))
        .await
        .expect("send manifest request");
    send.finish().expect("finish");
    let mut prefix = [0u8; 5];
    recv.read_exact(&mut prefix).await.expect("read prefix");
    let len = agent_share_proto::framing::decode_response_header(
        &prefix,
        agent_share_proto::framing::MAX_SIGNED_MANIFEST_BYTES,
    )
    .expect("manifest header");
    let mut bytes = vec![0u8; usize::try_from(len).expect("fits")];
    recv.read_exact(&mut bytes).await.expect("read manifest");
    // `OP_MANIFEST` answers with `version ‖ signature ‖ manifest`; these tests
    // are about which transport carried it, so the envelope is unwrapped and
    // the signature checked elsewhere.
    let signed = agent_share_proto::authorship::SignedManifest::decode(&bytes)
        .expect("decode the manifest envelope");
    MountManifest::decode(&signed.manifest).expect("decode manifest")
}

async fn read_range(
    conn: &fofoca::iroh::endpoint::Connection,
    secret: &[u8; SECRET_LEN],
    index: u32,
    offset: u64,
    len: u32,
) -> Vec<u8> {
    let (mut send, mut recv) = conn.open_bi().await.expect("open read stream");
    send.write_all(&agent_share_proto::framing::encode_read_request(
        secret, index, offset, len,
    ))
    .await
    .expect("send read request");
    send.finish().expect("finish");
    let mut prefix = [0u8; 5];
    recv.read_exact(&mut prefix).await.expect("read prefix");
    let got =
        agent_share_proto::framing::decode_response_header(&prefix, len).expect("read header");
    let mut data = vec![0u8; usize::try_from(got).expect("fits")];
    recv.read_exact(&mut data).await.expect("read body");
    data
}

async fn assert_hello_readable(
    conn: &fofoca::iroh::endpoint::Connection,
    secret: &[u8; SECRET_LEN],
) {
    let listing = fetch_manifest(conn, secret).await;
    assert_eq!(listing.files.len(), 1);
    let hello = listing
        .files
        .iter()
        .position(|file| file.rel_path == "hello.txt")
        .expect("hello.txt");
    let index = u32::try_from(hello).expect("index");
    assert_eq!(read_range(conn, secret, index, 0, 5).await, b"hello");
    assert_eq!(read_range(conn, secret, index, 6, 100).await, b"world");
}

/// `webrtc` mode: mount dial uses only the custom `WebRTC` addr.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn webrtc_mode_mounts_over_data_channel_only() {
    let tree = temp_tree();
    let secret = [11u8; SECRET_LEN];
    let (producer, producer_webrtc) =
        bind_webrtc(vec![MOUNT_ALPN.to_vec(), WEBRTC_SIGNAL_ALPN.to_vec()]).await;
    let producer_id = producer.id();
    let producer_addr = producer.addr();
    let server = spawn_signal_and_mount_server(
        producer.clone(),
        producer_webrtc,
        producer_id,
        secret,
        tree.clone(),
    );

    let (consumer, consumer_webrtc) = bind_webrtc(Vec::new()).await;
    let consumer_id = consumer.id();

    let signal = consumer
        .connect(producer_addr, WEBRTC_SIGNAL_ALPN)
        .await
        .expect("dial signal");
    let (mut send, mut recv) = signal.open_bi().await.expect("open signal");
    let (pending, offer) = offer_with(consumer_id, &ice()).await.expect("offer");
    send.write_all(&serde_json::to_vec(&offer).expect("encode"))
        .await
        .expect("send offer");
    send.finish().expect("finish");
    let raw = recv
        .read_to_end(MAX_ENVELOPE_BYTES)
        .await
        .expect("read answer");
    let answer: SignalEnvelope = serde_json::from_slice(&raw).expect("parse answer");
    let session = Box::pin(pending.complete(&answer, Duration::from_secs(20)))
        .await
        .expect("complete");
    consumer_webrtc
        .attach(producer_id, session)
        .expect("attach");
    signal.close(0u32.into(), b"jsep done");

    let webrtc_only = EndpointAddr::from_parts(
        producer_id,
        [TransportAddr::Custom(custom_addr(producer_id))],
    );
    let mount = consumer
        .connect(webrtc_only, MOUNT_ALPN)
        .await
        .expect("dial mount over WebRTC only");
    assert_hello_readable(&mount, &secret).await;

    mount.close(0u32.into(), b"done");
    producer.close().await;
    server.abort();
    let _ = std::fs::remove_dir_all(&tree);
}

/// **The native↔native pin: a data channel sitting right there must not win.**
///
/// `WebRTC` is the browser lane. Two native peers use iroh's own transports, and
/// nothing in this repo asserted that before — the closest test,
/// `the_mount_selects_webrtc_over_a_warm_relay_path` in `webrtc_mount.rs`,
/// deliberately clears IP so the contest is webrtc-vs-relay, a different
/// question.
///
/// So this sets up the adversarial case: a live, attached `WebRTC` session
/// *and* a reachable IP path, then dials the ordinary ticket address. The
/// selected path must be IP. Measured, the alternative is 6× less throughput at
/// 36× the latency, so a regression here is expensive and
/// completely silent — every byte still arrives.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_to_native_selects_ip_even_with_a_live_webrtc_session() {
    let tree = temp_tree();
    let secret = [17u8; SECRET_LEN];
    let (producer, producer_webrtc) =
        bind_webrtc(vec![MOUNT_ALPN.to_vec(), WEBRTC_SIGNAL_ALPN.to_vec()]).await;
    let producer_id = producer.id();
    let producer_addr = producer.addr();
    let server = spawn_signal_and_mount_server(
        producer.clone(),
        producer_webrtc,
        producer_id,
        secret,
        tree.clone(),
    );

    // A native consumer that *does* carry the lane — exactly what `mount`
    // builds, since the same handle is what the share mesh rides.
    let (consumer, consumer_webrtc) = bind_webrtc(Vec::new()).await;
    let consumer_id = consumer.id();

    // Negotiate a real session, so the custom addr is live before we dial.
    let signal = consumer
        .connect(producer_addr.clone(), WEBRTC_SIGNAL_ALPN)
        .await
        .expect("dial signal");
    let (mut send, mut recv) = signal.open_bi().await.expect("open signal");
    let (pending, offer) = offer_with(consumer_id, &ice()).await.expect("offer");
    send.write_all(&serde_json::to_vec(&offer).expect("encode"))
        .await
        .expect("send offer");
    send.finish().expect("finish");
    let raw = recv
        .read_to_end(MAX_ENVELOPE_BYTES)
        .await
        .expect("read answer");
    let answer: SignalEnvelope = serde_json::from_slice(&raw).expect("parse answer");
    let session = Box::pin(pending.complete(&answer, Duration::from_secs(20)))
        .await
        .expect("complete");
    consumer_webrtc
        .attach(producer_id, session)
        .expect("attach");
    signal.close(0u32.into(), b"jsep done");

    // The ordinary dial: the ticket address, which carries IP.
    let mount = consumer
        .connect(producer_addr, MOUNT_ALPN)
        .await
        .expect("dial mount over the ticket address");
    assert_hello_readable(&mount, &secret).await;

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if mount
            .paths()
            .iter()
            .any(|path| path.is_selected() && path.is_ip())
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "a native↔native mount must select an IP path, not the data channel \
             (paths={:?}); WebRTC is the browser lane — see mount/webrtc.rs",
            mount
                .paths()
                .iter()
                .map(|path| {
                    let kind = if path.is_relay() {
                        "relay"
                    } else if path.is_ip() {
                        "ip"
                    } else {
                        "webrtc-or-other"
                    };
                    if path.is_selected() {
                        format!("*{kind}")
                    } else {
                        kind.to_owned()
                    }
                })
                .collect::<Vec<_>>()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    mount.close(0u32.into(), b"done");
    producer.close().await;
    server.abort();
    let _ = std::fs::remove_dir_all(&tree);
}

/// `dynamic` fallback: when `WebRTC` cannot be used, mount still opens on the
/// ticket address (direct IP here; iroh relay in production).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dynamic_falls_back_to_ticket_addr_when_webrtc_is_skipped() {
    let tree = temp_tree();
    let secret = [13u8; SECRET_LEN];
    // Producer serves mount on IP; no usable WebRTC session for the consumer.
    let producer = bind_plain(vec![MOUNT_ALPN.to_vec()]).await;
    let producer_addr = producer.addr();
    let server = spawn_mount_server(producer.clone(), secret, tree.clone());

    let consumer = bind_plain(Vec::new()).await;

    // Analog of: WebRTC attempt failed → dial ticket.addr (relay/IP).
    let webrtc_only = EndpointAddr::from_parts(
        producer_addr.id,
        [TransportAddr::Custom(custom_addr(producer_addr.id))],
    );
    let webrtc_attempt = consumer.connect(webrtc_only, MOUNT_ALPN).await;
    assert!(
        webrtc_attempt.is_err(),
        "WebRTC-only dial must fail without an attached session",
    );

    let mount = consumer
        .connect(producer_addr, MOUNT_ALPN)
        .await
        .expect("dynamic fallback dial over ticket addr");
    assert_hello_readable(&mount, &secret).await;

    mount.close(0u32.into(), b"done");
    producer.close().await;
    server.abort();
    let _ = std::fs::remove_dir_all(&tree);
}
