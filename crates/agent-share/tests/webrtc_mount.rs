//! End-to-end: a share served over a real `WebRTC` data channel.
//!
//! This is the case the experiment this was ported from never covered. It
//! stands up a producer and a consumer as separate iroh endpoints, negotiates
//! JSEP between them, and then runs the *actual* mount protocol —
//! `OP_MANIFEST` and ranged `OP_READ`s — over the resulting data channel.
//!
//! The load-bearing detail is that the mount connection is dialled against an
//! address carrying **only** the `WebRTC` custom addr. iroh only fans a
//! connect's Initial out to candidate paths while the remote has no selected
//! path, so this is a fresh connection rather than an upgrade of the
//! signalling one — and because the address lists no IP or relay path, a pass
//! here cannot be a direct connection wearing a disguise.

use std::sync::Arc;
use std::time::Duration;

use agent_share_proto::framing::{MOUNT_ALPN, SECRET_LEN, WEBRTC_SIGNAL_ALPN};
use agent_share_proto::manifest::MountManifest;
use fofoca_iroh_webrtc_transport::{
    IceConfig, MAX_ENVELOPE_BYTES, SignalEnvelope, WebRtcHandle, WebRtcTransport, answer_with,
    custom_addr, offer_with,
};
use iroh::{Endpoint, EndpointAddr, SecretKey, TransportAddr, endpoint::presets};

/// Offline: the default config queries public STUN servers, which would make
/// this test depend on the network. Host candidates reach loopback fine.
fn ice() -> IceConfig {
    IceConfig::host_only()
}

fn temp_tree() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "agent-share-webrtc-e2e-{}",
        rand::RngCore::next_u64(&mut rand::rng())
    ));
    std::fs::create_dir_all(dir.join("docs")).expect("create fixture dirs");
    std::fs::write(dir.join("hello.txt"), b"hello world").expect("write hello");
    std::fs::write(dir.join("docs/guide.md"), b"lazy bytes").expect("write guide");
    dir
}

/// Bind an endpoint whose only custom transport is `WebRTC`, on a pinned key
/// so the transport advertises the identity the endpoint binds.
async fn endpoint_with_webrtc(alpns: Vec<Vec<u8>>) -> (Endpoint, WebRtcHandle) {
    let mut key_bytes = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rng(), &mut key_bytes);
    let key = SecretKey::from_bytes(&key_bytes);
    let handle = WebRtcHandle::new(WebRtcTransport::new(key.public()));
    let endpoint = Endpoint::builder(presets::Minimal)
        .secret_key(key)
        .relay_mode(iroh::RelayMode::Disabled)
        .clear_address_lookup()
        .alpns(alpns)
        .add_custom_transport(handle.transport())
        .bind()
        .await
        .expect("bind endpoint");
    (endpoint, handle)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_share_is_readable_over_a_webrtc_data_channel() {
    let tree = temp_tree();

    // ── producer ─────────────────────────────────────────────────────────
    let (manifest, paths) = agent_share::test_support::scan(&tree).expect("scan");
    let shared_tree = agent_share::test_support::live_tree(tree.clone(), manifest, paths);
    let secret = [7u8; SECRET_LEN];

    let (producer, producer_webrtc) =
        endpoint_with_webrtc(vec![MOUNT_ALPN.to_vec(), WEBRTC_SIGNAL_ALPN.to_vec()]).await;
    let producer_id = producer.id();
    let producer_addr = producer.addr();

    let accept_endpoint = producer.clone();
    let accept_webrtc = producer_webrtc.clone();
    let server = tokio::spawn(async move {
        while let Some(incoming) = accept_endpoint.accept().await {
            let shared_tree = Arc::clone(&shared_tree);
            let webrtc = accept_webrtc.clone();
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
                } else {
                    let _ = agent_share::test_support::serve_mount(conn, secret, shared_tree).await;
                }
            });
        }
    });

    // ── consumer ─────────────────────────────────────────────────────────
    let (consumer, consumer_webrtc) = endpoint_with_webrtc(Vec::new()).await;
    let consumer_id = consumer.id();

    // 1. Signal over the ordinary (here, direct) path.
    let signal = consumer
        .connect(producer_addr, WEBRTC_SIGNAL_ALPN)
        .await
        .expect("dial signal ALPN");
    let (mut send, mut recv) = signal.open_bi().await.expect("open signal stream");
    let (pending, offer) = offer_with(consumer_id, &ice()).await.expect("build offer");
    send.write_all(&serde_json::to_vec(&offer).expect("encode offer"))
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
        .expect("complete offer");
    consumer_webrtc
        .attach(producer_id, session)
        .expect("attach session");
    signal.close(0u32.into(), b"jsep done");

    // 2. A *fresh* dial over an address that lists the data channel and
    //    nothing else, so a pass cannot be a direct connection in disguise.
    let webrtc_only = EndpointAddr::from_parts(
        producer_id,
        [TransportAddr::Custom(custom_addr(producer_id))],
    );
    let mount = consumer
        .connect(webrtc_only, MOUNT_ALPN)
        .await
        .expect("dial the mount ALPN over WebRTC");

    // 3. The real protocol.
    let listing = fetch_manifest(&mount, &secret).await;
    assert_eq!(listing.files.len(), 2, "both files listed");
    assert_eq!(listing.dirs.len(), 1, "the docs dir listed");

    let hello = listing
        .files
        .iter()
        .position(|file| file.rel_path == "hello.txt")
        .expect("hello.txt in the manifest");
    let index = u32::try_from(hello).expect("index fits u32");

    assert_eq!(read_range(&mount, &secret, index, 0, 5).await, b"hello");
    assert_eq!(read_range(&mount, &secret, index, 6, 100).await, b"world");
    assert!(
        read_range(&mount, &secret, index, 999, 4).await.is_empty(),
        "a past-EOF read is a valid empty read, not an error"
    );

    mount.close(0u32.into(), b"done");
    producer.close().await;
    server.abort();
    let _ = std::fs::remove_dir_all(&tree);
}

async fn fetch_manifest(
    conn: &iroh::endpoint::Connection,
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
        agent_share_proto::framing::MAX_MANIFEST_BYTES,
    )
    .expect("manifest header");
    let mut bytes = vec![0u8; usize::try_from(len).expect("fits")];
    recv.read_exact(&mut bytes).await.expect("read manifest");
    MountManifest::decode(&bytes).expect("decode manifest")
}

async fn read_range(
    conn: &iroh::endpoint::Connection,
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

// ── the two lanes share one registry ────────────────────────────────────────
//
// Since the share and the mesh were put on one endpoint, the mount lane and the
// mesh lane attach into the same session registry, and a registry that already
// holds a session for a peer refuses a second one. These pin the behaviour that
// makes that survivable: ask the registry first, defer to it after.

/// Attach a real session for the pair, out of band — standing in for whatever
/// the mesh lane would have done.
async fn attach_pair(
    offerer_id: iroh::EndpointId,
    offerer: &WebRtcHandle,
    answerer_id: iroh::EndpointId,
    answerer: &WebRtcHandle,
) {
    let (pending_offer, offer) = offer_with(offerer_id, &ice()).await.expect("build offer");
    let (pending_answer, answer) = answer_with(answerer_id, &offer, &ice())
        .await
        .expect("build answer");
    let (offerer_session, answerer_session) = tokio::join!(
        Box::pin(pending_offer.complete(&answer, Duration::from_secs(20))),
        Box::pin(pending_answer.complete(Duration::from_secs(20))),
    );
    offerer
        .attach(answerer_id, offerer_session.expect("offerer session"))
        .expect("attach offerer");
    answerer
        .attach(offerer_id, answerer_session.expect("answerer session"))
        .expect("attach answerer");
}

/// With a session already in the registry, `dial_webrtc` must hand back the
/// custom-addr-only address without signalling at all.
///
/// The counter is the assertion that matters. Before this, losing the race to
/// the mesh cost a full 20s JSEP round that ended in "a live session already
/// exists" and dropped the mount to relay.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dial_webrtc_reuses_a_session_another_lane_attached() {
    let (producer, producer_webrtc) =
        endpoint_with_webrtc(vec![MOUNT_ALPN.to_vec(), WEBRTC_SIGNAL_ALPN.to_vec()]).await;
    let (consumer, consumer_webrtc) = endpoint_with_webrtc(Vec::new()).await;
    let producer_id = producer.id();
    let consumer_id = consumer.id();

    let rounds = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let accept_endpoint = producer.clone();
    let counted = Arc::clone(&rounds);
    let server = tokio::spawn(async move {
        while let Some(incoming) = accept_endpoint.accept().await {
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let Ok(conn) = incoming.await else { continue };
            conn.close(0u32.into(), b"unexpected");
        }
    });

    // Boxed: two full JSEP rounds' worth of str0m state on this future.
    Box::pin(attach_pair(
        consumer_id,
        &consumer_webrtc,
        producer_id,
        &producer_webrtc,
    ))
    .await;

    let addr = Box::pin(agent_share::test_support::dial_webrtc(
        &consumer,
        producer.addr(),
        &consumer_webrtc,
        &ice(),
    ))
    .await
    .expect("dial_webrtc must succeed against an existing session");

    assert_eq!(addr.id, producer_id);
    let only_webrtc = addr
        .addrs
        .iter()
        .all(|addr| matches!(addr, TransportAddr::Custom(_)));
    assert!(only_webrtc, "must return a webrtc-only address: {addr:?}");
    assert_eq!(
        rounds.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "no signalling connection should have been opened at all"
    );

    server.abort();
    consumer.close().await;
    producer.close().await;
}

/// The answering half: a producer that already holds a session refuses the
/// round with an `Error` envelope, before spending anything on ICE.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn serve_signal_refuses_when_a_session_already_exists() {
    let (producer, producer_webrtc) = endpoint_with_webrtc(vec![WEBRTC_SIGNAL_ALPN.to_vec()]).await;
    let (consumer, consumer_webrtc) = endpoint_with_webrtc(Vec::new()).await;
    let producer_id = producer.id();
    let consumer_id = consumer.id();
    let producer_addr = producer.addr();

    // Boxed: two full JSEP rounds' worth of str0m state on this future.
    Box::pin(attach_pair(
        consumer_id,
        &consumer_webrtc,
        producer_id,
        &producer_webrtc,
    ))
    .await;

    let accept_endpoint = producer.clone();
    let accept_webrtc = producer_webrtc.clone();
    let server = tokio::spawn(async move {
        let Some(incoming) = accept_endpoint.accept().await else {
            return;
        };
        let conn = incoming.await.expect("accept signal connection");
        agent_share::test_support::serve_signal(&conn, producer_id, &accept_webrtc, &ice())
            .await
            .expect("serve_signal must not error on a refusal");
    });

    // Dial by hand: `dial_webrtc` would short-circuit on its own `has_session`
    // check, and the point here is what the *answerer* puts on the wire.
    let signal = consumer
        .connect(producer_addr, WEBRTC_SIGNAL_ALPN)
        .await
        .expect("dial signal ALPN");
    let (mut send, mut recv) = signal.open_bi().await.expect("open signal stream");
    let (_pending, offer) = offer_with(consumer_id, &ice()).await.expect("build offer");
    send.write_all(&serde_json::to_vec(&offer).expect("encode offer"))
        .await
        .expect("send offer");
    send.finish().expect("finish");
    let raw = recv
        .read_to_end(MAX_ENVELOPE_BYTES)
        .await
        .expect("read reply");
    let reply: SignalEnvelope = serde_json::from_slice(&raw).expect("parse reply");

    assert!(
        matches!(reply, SignalEnvelope::Error { .. }),
        "expected a refusal, got {reply:?}"
    );
    assert!(
        producer_webrtc.has_session(&consumer_id),
        "the existing session must be untouched"
    );

    signal.close(0u32.into(), b"done");
    server.await.expect("server task");
    consumer.close().await;
    producer.close().await;
}
