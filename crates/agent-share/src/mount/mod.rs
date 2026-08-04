mod bench;
mod consume;
mod handlers;
mod hash;
mod live;
mod mesh;
mod mirror;
mod nfs;
mod produce;
mod scan;
mod webrtc;

/// Re-exports for the integration tests; see `crate::test_support`.
///
/// `pub` inside a `pub(crate)` module: unreachable on its own, which is the
/// point — `crate::test_support` is the only door, and it is `#[doc(hidden)]`.
#[expect(unreachable_pub, reason = "re-exported through crate::test_support")]
#[doc(hidden)]
pub mod test_support {
    use std::path::PathBuf;
    use std::sync::Arc;

    use agent_share_proto::manifest::MountManifest;
    use anyhow::Result;

    pub use super::live::LiveTree;
    pub use super::produce::serve_established as serve_mount;
    pub use super::scan::scan;
    /// The two halves of the share's `WebRTC` lane, so a test can drive the real
    /// ones rather than a hand-rolled copy. The hand-rolled copy in
    /// `tests/webrtc_mount.rs` is what let the registry-collision bug live: it
    /// exercised the shape of the lane, not the lane.
    pub use super::webrtc::{dial_webrtc, serve_signal};

    /// Build the producer's tree from a scan, for tests that stand a producer
    /// up by hand. `paths` must stay in the order [`scan`] returned them: a
    /// file's index in the manifest *is* its READ address, so a different
    /// order would silently corrupt every read.
    ///
    /// No watcher is attached — these trees are as static as the old snapshot
    /// was, which is what a test wants.
    #[must_use]
    pub fn live_tree(root: PathBuf, manifest: MountManifest, paths: Vec<PathBuf>) -> Arc<LiveTree> {
        Arc::new(LiveTree::new(root, manifest, paths))
    }

    /// Keeps `Result` in scope for the re-exported server signature.
    #[expect(dead_code, reason = "documents the re-exported fn's error type")]
    type ServeResult = Result<()>;
    /// Keeps `Arc` in scope for the re-exported server signature.
    #[expect(dead_code, reason = "documents the re-exported fn's argument type")]
    type Shared<T> = Arc<T>;
}

pub(crate) use bench::{produce as produce_bench, run as run_bench};
pub(crate) use consume::attach;
pub(crate) use mirror::mirror;
pub(crate) use produce::serve;

// The mount protocol's identity, op codes, caps, manifest types and ticket
// codec live in `agent-share-proto` so the browser client links the very same
// bytes rather than a second implementation that drifts. Re-exported here
// under their long-standing names; the golden pin that guards them moved with
// them (`agent_share_proto::framing` — `wire_constants_are_pinned`).
pub(crate) use agent_share_proto::framing::{
    MAX_DELTA_BYTES, MAX_MANIFEST_BYTES, MAX_OUTBOARD_BYTES, MAX_READ_LEN, MOUNT_ALPN, OP_BENCH,
    OP_HASH, OP_MANIFEST, OP_READ, OP_WATCH, REQUEST_HEADER_LEN, SECRET_LEN, WATCH_FRAME_DELTA,
    WATCH_FRAME_MANIFEST,
};
pub(crate) use agent_share_proto::manifest::{MountManifest, ReadStatus};
pub(crate) use agent_share_proto::ticket::MountTicket;
pub(crate) use webrtc::{WEBRTC_SIGNAL_ALPN, dial_webrtc, serve_signal};

// The pre-ticket online wait is identical for every direct off-gossip
// command — reuse `file`'s rather than keeping a fourth copy.
use crate::file::wait_online;

/// Quote one word of a printed, copy-pastable command: plain when every
/// character is clearly shell-safe, single-quoted (embedded `'` escaped
/// POSIX-style) otherwise — an unquoted path with a space would split into
/// two arguments when pasted.
fn shell_word(raw: &str) -> String {
    let plain = !raw.is_empty()
        && raw
            .chars()
            .all(|ch| ch.is_alphanumeric() || matches!(ch, '/' | '.' | '_' | '-' | '~'));
    if plain {
        raw.to_owned()
    } else {
        format!("'{}'", raw.replace('\'', "'\\''"))
    }
}

/// Present the producer's status and the consumer's ready-to-run command on
/// **stdout** — the producer's product (file bytes flow over the network, not
/// stdout), and stderr stays errors-only. Human (default) is cargo-style
/// (`Serving <path>` / `Mount <command>`); `json` is the bare command for
/// machines (no status/colors), unchanged so scripts can capture it.
fn announce(json: bool, serving: &str, command: &str) {
    tracing::info!("serving {serving}");
    if json {
        println!("{command}");
        return;
    }
    crate::util::output::status_out("Serving", serving);
    crate::util::output::status_out("Mount", command);
}

#[cfg(test)]
mod tests {
    use super::consume::RemoteClient;
    use super::{MAX_READ_LEN, MountTicket, SECRET_LEN, produce};
    use crate::lookup::{add_peer_addr, build_endpoint};
    use crate::protocol::swarm::LookupOpts;
    use rand::RngCore;
    use std::path::PathBuf;
    use std::sync::Arc;

    /// A throwaway directory under the OS temp dir (the repo has no `tempfile`
    /// dep); dropped recursively at the end of each test.
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("agent-share-test-{}", rand::rng().next_u64()));
            std::fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    /// Stand up a loopback producer serving `root` and a client connected to
    /// it. The producer task accepts connections until its endpoint closes.
    async fn producer_and_client(
        root: &std::path::Path,
    ) -> (iroh::Endpoint, RemoteClient, tokio::task::JoinHandle<()>) {
        let (manifest, paths) = super::scan::scan(root).expect("scan");
        let tree = Arc::new(super::live::LiveTree::new(
            root.to_path_buf(),
            manifest,
            paths,
        ));
        let (endpoint, ticket, secret, _webrtc) = produce::bind(LookupOpts::loopback())
            .await
            .expect("bind producer");
        let accept_endpoint = endpoint.clone();
        let producer = tokio::spawn(async move {
            while let Some(incoming) = accept_endpoint.accept().await {
                let tree = Arc::clone(&tree);
                tokio::spawn(async move {
                    let Ok(conn) = incoming.await else { return };
                    let _ = produce::serve_established(conn, secret, tree, None).await;
                });
            }
        });

        let consumer_endpoint =
            build_endpoint(&ticket.lookups, None, None, Vec::new(), None, false)
                .await
                .expect("consumer endpoint");
        add_peer_addr(&consumer_endpoint, ticket.addr.clone()).expect("add peer addr");
        let client = RemoteClient::new(consumer_endpoint, ticket);
        (endpoint, client, producer)
    }

    /// As [`producer_and_client`], but with an explicit hash cache — `None`
    /// standing for a producer that cannot vouch for anything.
    async fn producer_with_hashes(
        root: &std::path::Path,
        hashes: Option<Arc<super::hash::HashCache>>,
    ) -> (iroh::Endpoint, RemoteClient, tokio::task::JoinHandle<()>) {
        let (manifest, paths) = super::scan::scan(root).expect("scan");
        let tree = Arc::new(super::live::LiveTree::new(
            root.to_path_buf(),
            manifest,
            paths,
        ));
        let (endpoint, ticket, secret, _webrtc) = produce::bind(LookupOpts::loopback())
            .await
            .expect("bind producer");
        let accept_endpoint = endpoint.clone();
        let producer = tokio::spawn(async move {
            while let Some(incoming) = accept_endpoint.accept().await {
                let tree = Arc::clone(&tree);
                let hashes = hashes.clone();
                tokio::spawn(async move {
                    let Ok(conn) = incoming.await else { return };
                    let _ = produce::serve_established(conn, secret, tree, hashes).await;
                });
            }
        });

        let consumer_endpoint =
            build_endpoint(&ticket.lookups, None, None, Vec::new(), None, false)
                .await
                .expect("consumer endpoint");
        add_peer_addr(&consumer_endpoint, ticket.addr.clone()).expect("add peer addr");
        let client = RemoteClient::new(consumer_endpoint, ticket);
        (endpoint, client, producer)
    }

    /// Stand up a producer serving `root` under a **caller-supplied** secret
    /// rather than the one `bind` mints for it.
    ///
    /// This is the re-seeder shape exactly: a peer that holds the bytes serves
    /// them under the *origin's* ticket secret. `serve_established` already
    /// takes the secret as a parameter, so no production code moves to make
    /// this possible — which is the claim under test.
    async fn producer_under_secret(
        root: &std::path::Path,
        secret: [u8; SECRET_LEN],
    ) -> (iroh::Endpoint, MountTicket, tokio::task::JoinHandle<()>) {
        let (manifest, paths) = super::scan::scan(root).expect("scan");
        let tree = Arc::new(super::live::LiveTree::new(
            root.to_path_buf(),
            manifest,
            paths,
        ));
        let (endpoint, mut ticket, _minted, _webrtc) = produce::bind(LookupOpts::loopback())
            .await
            .expect("bind re-seeder");
        // Advertise the origin's secret, not the freshly minted one.
        ticket.secret = secret;
        let accept_endpoint = endpoint.clone();
        let task = tokio::spawn(async move {
            while let Some(incoming) = accept_endpoint.accept().await {
                let tree = Arc::clone(&tree);
                tokio::spawn(async move {
                    let Ok(conn) = incoming.await else { return };
                    let _ = produce::serve_established(conn, secret, tree, None).await;
                });
            }
        });
        (endpoint, ticket, task)
    }

    /// Build a client that talks to `ticket`'s address using `ticket`'s secret.
    async fn client_for(ticket: MountTicket) -> RemoteClient {
        let endpoint = build_endpoint(&ticket.lookups, None, None, Vec::new(), None, false)
            .await
            .expect("client endpoint");
        add_peer_addr(&endpoint, ticket.addr.clone()).expect("add peer addr");
        RemoteClient::new(endpoint, ticket)
    }

    /// **Stage 3, end to end.** A consumer asks the origin for a file's root
    /// over `OP_HASH`, then checks the file's actual bytes against it.
    ///
    /// This is the whole trust chain in one test: the root comes from the
    /// origin over a channel authenticated to the ticket's endpoint id, and
    /// afterwards the *bytes* can come from anyone, because they either verify
    /// against that root or they do not.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_consumer_learns_a_root_and_the_bytes_verify_against_it() {
        let tree = fixture_tree();
        let contents = vec![9u8; 200_000];
        std::fs::write(tree.path.join("big.bin"), &contents).expect("write");

        let cache_dir = TempDir::new();
        let cache = Arc::new(super::hash::HashCache::open(&cache_dir.path).expect("cache"));
        let (endpoint, client, producer) =
            producer_with_hashes(&tree.path, Some(Arc::clone(&cache))).await;

        let manifest = client.fetch_manifest().await.expect("manifest");
        let index = manifest
            .files
            .iter()
            .position(|file| file.rel_path == "big.bin")
            .expect("big.bin listed");
        let index = u32::try_from(index).expect("index");

        let (root, outboard) = client
            .fetch_hash(index)
            .await
            .expect("hash request")
            .expect("the origin can vouch for this file");
        assert!(!outboard.is_empty(), "200 KB needs a tree");

        // The bytes verify against the root the origin gave us.
        let all = fofoca_blobs::ChunkRanges::all();
        let encoded = fofoca_blobs::encode_ranges(&contents, &all).expect("encode");
        let mut target = Vec::new();
        fofoca_blobs::decode_into(root, contents.len() as u64, &encoded, &all, &mut target)
            .expect("the file's own bytes must verify against its root");
        assert_eq!(target, contents);

        // And content that is *not* this file does not, which is the half that
        // makes the first half worth anything.
        let impostor = vec![8u8; 200_000];
        let forged = fofoca_blobs::encode_ranges(&impostor, &all).expect("encode");
        let mut wrong = Vec::new();
        assert!(
            fofoca_blobs::decode_into(root, impostor.len() as u64, &forged, &all, &mut wrong)
                .is_err(),
            "substituted content must not verify against the origin's root"
        );

        endpoint.close().await;
        producer.abort();
    }

    /// A producer with no hash cache answers `BadIndex`, and the consumer reads
    /// that as "cannot vouch" rather than as a failure.
    ///
    /// The fallback RFC 01 phase 4 requires: a file with no hash is read from
    /// the origin exactly as it is today.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_producer_without_a_cache_says_it_cannot_vouch() {
        let tree = fixture_tree();
        let (endpoint, client, producer) = producer_with_hashes(&tree.path, None).await;

        assert_eq!(
            client.fetch_hash(0).await.expect("hash request"),
            None,
            "no cache must read as 'cannot vouch', not as an error"
        );
        // And the ordinary read path is untouched.
        assert_eq!(client.read_range(0, 0, 5).await.expect("read").len(), 5);

        endpoint.close().await;
        producer.abort();
    }

    /// **S0.2 — the symmetry claim RFC 01 rests on.**
    ///
    /// `produce.rs`'s authentication is one line — `&header[..SECRET_LEN] !=
    /// secret` — with no binding to the serving endpoint's identity. So a peer
    /// that is *not* the origin can serve the origin's ticket, and a consumer
    /// cannot tell the difference. That is what makes a swarm possible with no
    /// new auth code; if it were false, every phase after this gets more
    /// expensive.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_non_origin_peer_serves_the_origins_ticket_secret() {
        // One secret, two independent hosts. Minting it here rather than
        // taking the origin's makes the point explicit: the secret is the
        // whole capability, and it is not bound to who serves it.
        let mut secret = [0u8; SECRET_LEN];
        rand::rng().fill_bytes(&mut secret);

        let origin_tree = fixture_tree();
        let (origin_endpoint, origin_ticket, origin_task) =
            producer_under_secret(&origin_tree.path, secret).await;

        // A second host with its own endpoint and its own copy of the bytes,
        // serving under the origin's secret. A mirror, in other words.
        let mirror_tree = fixture_tree();
        let (mirror_endpoint, mirror_ticket, mirror_task) =
            producer_under_secret(&mirror_tree.path, secret).await;
        assert_ne!(
            mirror_ticket.addr, origin_ticket.addr,
            "the mirror must be a genuinely different endpoint"
        );

        // A consumer pointed at the mirror, holding only the origin's ticket
        // secret, is served — no new code anywhere.
        let mirror_client = client_for(mirror_ticket).await;
        let manifest = mirror_client
            .fetch_manifest()
            .await
            .expect("a peer must serve the origin's secret");
        let hello = manifest
            .files
            .iter()
            .position(|file| file.rel_path == "hello.txt")
            .expect("hello.txt listed by the mirror");
        let bytes = mirror_client
            .read_range(u32::try_from(hello).expect("index"), 0, 5)
            .await
            .expect("ranged read from a non-origin peer");
        assert_eq!(&bytes, b"hello");

        origin_endpoint.close().await;
        mirror_endpoint.close().await;
        origin_task.abort();
        mirror_task.abort();
    }

    /// **The other half of S0.2: the danger is real too.**
    ///
    /// A re-seeder whose tree has *diverged* from the origin's answers
    /// plausibly and wrongly — same index, different bytes, no error. This is
    /// why RFC 01's guard #1 (a manifest fingerprint on every card) is not
    /// optional and not retrofittable. Pinned here as a failing-by-construction
    /// demonstration so the guard cannot be quietly dropped later.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_diverged_peer_answers_plausibly_and_wrongly() {
        let mut secret = [0u8; SECRET_LEN];
        rand::rng().fill_bytes(&mut secret);

        let origin_tree = fixture_tree();
        let (origin_endpoint, origin_ticket, origin_task) =
            producer_under_secret(&origin_tree.path, secret).await;
        let origin_client = client_for(origin_ticket).await;

        // Same shape, different contents — a stale mirror.
        let stale = TempDir::new();
        std::fs::create_dir_all(stale.path.join("docs")).unwrap();
        std::fs::write(stale.path.join("hello.txt"), b"WRONG WORLD").unwrap();
        std::fs::write(stale.path.join("docs/guide.md"), b"stale bytes").unwrap();

        let (stale_endpoint, stale_ticket, stale_task) =
            producer_under_secret(&stale.path, secret).await;
        let stale_client = client_for(stale_ticket).await;

        let origin_manifest = origin_client
            .fetch_manifest()
            .await
            .expect("origin manifest");
        let stale_manifest = stale_client.fetch_manifest().await.expect("stale manifest");
        let idx = origin_manifest
            .files
            .iter()
            .position(|file| file.rel_path == "hello.txt")
            .expect("hello.txt");
        let idx = u32::try_from(idx).expect("index");

        let good = origin_client
            .read_range(idx, 0, 5)
            .await
            .expect("origin read");
        let bad = stale_client
            .read_range(idx, 0, 5)
            .await
            .expect("stale read");

        assert_eq!(&good, b"hello");
        assert_ne!(
            good, bad,
            "the stale peer returned different bytes for the same index — \
             it succeeded, which is exactly the silent-corruption class guard #1 exists to stop"
        );
        assert_ne!(
            origin_manifest.encode(),
            stale_manifest.encode(),
            "a manifest fingerprint must be able to tell these two trees apart"
        );

        origin_endpoint.close().await;
        stale_endpoint.close().await;
        origin_task.abort();
        stale_task.abort();
    }

    fn fixture_tree() -> TempDir {
        let tmp = TempDir::new();
        std::fs::create_dir_all(tmp.path.join("docs")).unwrap();
        std::fs::write(tmp.path.join("hello.txt"), b"hello world").unwrap();
        std::fs::write(tmp.path.join("docs/guide.md"), b"lazy bytes").unwrap();
        tmp
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn manifest_and_ranged_reads_round_trip() {
        let tree = fixture_tree();
        let (endpoint, client, producer) = producer_and_client(&tree.path).await;

        let manifest = client.fetch_manifest().await.expect("manifest");
        assert_eq!(manifest.files.len(), 2);
        assert_eq!(manifest.dirs.len(), 1);
        let hello = manifest
            .files
            .iter()
            .position(|file| file.rel_path == "hello.txt")
            .expect("hello.txt listed");
        let hello = u32::try_from(hello).expect("index");

        let head = client.read_range(hello, 0, 5).await.expect("read");
        assert_eq!(head, b"hello");
        let tail = client.read_range(hello, 6, 100).await.expect("read tail");
        assert_eq!(tail, b"world");
        let past = client.read_range(hello, 999, 4).await.expect("past eof");
        assert!(past.is_empty(), "past-EOF read is a valid empty read");

        endpoint.close().await;
        producer.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bad_index_and_oversize_len_error_without_killing_the_connection() {
        let tree = fixture_tree();
        let (endpoint, client, producer) = producer_and_client(&tree.path).await;

        assert!(client.read_range(424_242, 0, 4).await.is_err(), "bad index");
        assert!(
            client.read_range(0, 0, MAX_READ_LEN + 1).await.is_err(),
            "len over cap"
        );
        // The connection survived both rejections.
        let data = client.read_range(0, 0, 4).await.expect("still serving");
        assert_eq!(data.len(), 4);

        endpoint.close().await;
        producer.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bad_secret_is_rejected() {
        let tree = fixture_tree();
        let (endpoint, client, producer) = producer_and_client(&tree.path).await;

        // A ticket with a corrupted bearer secret: the producer closes the
        // connection, so the request fails rather than answering.
        let bad_ticket = MountTicket {
            addr: client.producer_addr(),
            secret: [0u8; SECRET_LEN],
            lookups: LookupOpts::loopback(),
            kind: agent_share_proto::ticket::TICKET_KIND_SHARE,
        };
        let bad_endpoint = build_endpoint(&bad_ticket.lookups, None, None, Vec::new(), None, false)
            .await
            .expect("bad-client endpoint");
        add_peer_addr(&bad_endpoint, bad_ticket.addr.clone()).expect("add peer addr");
        let bad = RemoteClient::new(bad_endpoint, bad_ticket);
        assert!(bad.fetch_manifest().await.is_err(), "bad secret must fail");

        // The honest client still works — the producer keeps accepting.
        assert!(client.fetch_manifest().await.is_ok());

        endpoint.close().await;
        producer.abort();
    }
}
