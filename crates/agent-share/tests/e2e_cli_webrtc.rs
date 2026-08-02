//! End to end against the **real binary**.
//!
//! Spawns `agent-share serve` as a subprocess, scrapes the ticket it
//! prints, and reads the share back over a `WebRTC` data channel — the same path
//! the browser takes, minus the browser.
//!
//! This is the test that would have caught every integration bug the unit
//! suites cannot see: that `serve` actually registers the signal ALPN, that the
//! transport advertises the identity the endpoint binds (a mismatch there is
//! silent — the endpoint comes up fine and every dial goes to an address nobody
//! listens on), and that a ticket minted by the producer decodes into something
//! dialable.
//!
//! Ignored by default: it builds and spawns the binary, and `serve` needs
//! mDNS/relay reach, which is environment-dependent. Run with
//! `cargo test --test e2e_cli_webrtc -- --ignored --nocapture`.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use agent_share_proto::framing::{self, MAX_MANIFEST_BYTES, MOUNT_ALPN, WEBRTC_SIGNAL_ALPN};
use agent_share_proto::manifest::MountManifest;
use agent_share_proto::ticket::MountTicket;
use fofoca_iroh_webrtc_transport::{
    IceConfig, MAX_ENVELOPE_BYTES, SignalEnvelope, WebRtcHandle, WebRtcTransport, custom_addr,
    offer_with,
};
use iroh::endpoint::{Connection, presets};
use iroh::{Endpoint, EndpointAddr, SecretKey, TransportAddr};

/// Kills the child on drop, so a failing assert does not leave a daemon behind.
struct Serving(Child);

impl Drop for Serving {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn fixture() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "agent-share-cli-e2e-{}",
        rand::RngCore::next_u64(&mut rand::rng())
    ));
    std::fs::create_dir_all(dir.join("docs")).expect("fixture dirs");
    std::fs::write(dir.join("hello.txt"), b"hello from agent-share").expect("hello");
    // Larger than one 256 KiB read, so the chunking path is exercised rather
    // than assumed.
    std::fs::write(dir.join("docs/blob.bin"), vec![0xABu8; 300_000]).expect("blob");
    dir
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "spawns the real binary and needs network reach; run with --ignored"]
async fn the_real_cli_serves_over_webrtc() {
    let tree = fixture();

    let mut child = Command::new(env!("CARGO_BIN_EXE_agent-share"))
        .args(["serve", tree.to_str().expect("utf-8 path"), "--mdns"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn agent-share serve");
    let stdout = child.stdout.take().expect("piped stdout");
    let serving = Serving(child);

    // Scrape the ticket off the `Mount agent-share <ticket> .` line. The
    // ticket is bare Base58 with nothing to grep for, so the anchor is the
    // literal command word and the proof is that the next word decodes.
    let ticket = tokio::task::spawn_blocking(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let mut words = line.split_whitespace();
            while let Some(word) = words.next() {
                if word != "agent-share" {
                    continue;
                }
                if let Some(candidate) = words.next()
                    && MountTicket::decode(candidate).is_ok()
                {
                    return Some(candidate.to_owned());
                }
            }
        }
        None
    })
    .await
    .expect("scrape task")
    .expect("serve printed a ticket");

    let ticket = MountTicket::decode(&ticket).expect("the scraped ticket decodes");
    let producer = ticket.addr.id;

    // A consumer whose only custom transport is WebRTC.
    let key = SecretKey::generate();
    let local = key.public();
    let handle = WebRtcHandle::new(WebRtcTransport::new(local));
    let consumer = Endpoint::builder(presets::Minimal)
        .secret_key(key)
        .relay_mode(iroh::RelayMode::Disabled)
        .add_custom_transport(handle.transport())
        .bind()
        .await
        .expect("bind consumer");
    agent_share::test_support::add_peer_addr(&consumer, ticket.addr.clone())
        .expect("register the producer's address");

    // 1. Signal.
    let signal = consumer
        .connect(ticket.addr.clone(), WEBRTC_SIGNAL_ALPN)
        .await
        .expect("dial the signal ALPN on the real producer");
    let (mut send, mut recv) = signal.open_bi().await.expect("open signal stream");
    let (pending, offer) = offer_with(local, &IceConfig::host_only())
        .await
        .expect("build offer");
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
        .expect("complete negotiation with the real producer");
    handle.attach(producer, session).expect("attach");
    signal.close(0u32.into(), b"jsep done");

    // 2. Fresh dial, WebRTC-only address.
    let mount = consumer
        .connect(
            EndpointAddr::from_parts(producer, [TransportAddr::Custom(custom_addr(producer))]),
            MOUNT_ALPN,
        )
        .await
        .expect("dial the mount ALPN over the data channel");

    // 3. The real protocol against the real producer.
    let manifest = manifest(&mount, &ticket.secret).await;
    assert_eq!(manifest.files.len(), 2, "hello.txt and docs/blob.bin");

    let hello = index_of(&manifest, "hello.txt");
    assert_eq!(
        read(&mount, &ticket.secret, hello, 0, 64).await,
        b"hello from agent-share"
    );

    // Chunked: 300 KB does not fit one 256 KiB read.
    let blob = index_of(&manifest, "docs/blob.bin");
    let mut got = Vec::new();
    while (got.len() as u64) < 300_000 {
        let chunk = read(
            &mount,
            &ticket.secret,
            blob,
            got.len() as u64,
            framing::MAX_READ_LEN,
        )
        .await;
        assert!(!chunk.is_empty(), "a short read before EOF");
        got.extend_from_slice(&chunk);
    }
    assert_eq!(got.len(), 300_000, "the whole file came back");
    assert!(got.iter().all(|&byte| byte == 0xAB), "bytes are intact");

    mount.close(0u32.into(), b"done");
    drop(serving);
    let _ = std::fs::remove_dir_all(&tree);
}

fn index_of(manifest: &MountManifest, path: &str) -> u32 {
    u32::try_from(
        manifest
            .files
            .iter()
            .position(|file| file.rel_path == path)
            .unwrap_or_else(|| panic!("{path} is in the manifest")),
    )
    .expect("index fits u32")
}

async fn manifest(conn: &Connection, secret: &[u8; 32]) -> MountManifest {
    let (mut send, mut recv) = conn.open_bi().await.expect("open");
    send.write_all(&framing::encode_manifest_request(secret))
        .await
        .expect("send");
    send.finish().expect("finish");
    let mut prefix = [0u8; 5];
    recv.read_exact(&mut prefix).await.expect("prefix");
    let len = framing::decode_response_header(&prefix, MAX_MANIFEST_BYTES).expect("header");
    let mut bytes = vec![0u8; len as usize];
    recv.read_exact(&mut bytes).await.expect("body");
    MountManifest::decode(&bytes).expect("decode")
}

async fn read(conn: &Connection, secret: &[u8; 32], index: u32, offset: u64, len: u32) -> Vec<u8> {
    let (mut send, mut recv) = conn.open_bi().await.expect("open");
    send.write_all(&framing::encode_read_request(secret, index, offset, len))
        .await
        .expect("send");
    send.finish().expect("finish");
    let mut prefix = [0u8; 5];
    recv.read_exact(&mut prefix).await.expect("prefix");
    let got = framing::decode_response_header(&prefix, len).expect("header");
    let mut data = vec![0u8; got as usize];
    recv.read_exact(&mut data).await.expect("body");
    data
}
