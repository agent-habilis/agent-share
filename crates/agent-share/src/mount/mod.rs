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
mod sources;
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
    use agent_share_proto::auth::{ShareAuth, share_token};
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
    ) -> (
        fofoca::iroh::Endpoint,
        RemoteClient,
        tokio::task::JoinHandle<()>,
    ) {
        let (manifest, paths) = super::scan::scan(root).expect("scan");
        let tree = Arc::new(super::live::LiveTree::new(
            root.to_path_buf(),
            manifest,
            paths,
        ));
        let (endpoint, ticket, secret, _webrtc) = produce::bind(LookupOpts::loopback(), None)
            .await
            .expect("bind producer");
        let auth = ShareAuth::new(&secret, None);
        let accept_endpoint = endpoint.clone();
        let producer = tokio::spawn(async move {
            while let Some(incoming) = accept_endpoint.accept().await {
                let tree = Arc::clone(&tree);
                tokio::spawn(async move {
                    let Ok(conn) = incoming.await else { return };
                    let _ = produce::serve_established(conn, auth, tree, None).await;
                });
            }
        });

        let consumer_endpoint =
            build_endpoint(&ticket.lookups, None, None, Vec::new(), None, false)
                .await
                .expect("consumer endpoint");
        add_peer_addr(&consumer_endpoint, ticket.addr.clone()).expect("add peer addr");
        let client = RemoteClient::new(consumer_endpoint, ticket, auth);
        (endpoint, client, producer)
    }

    /// As [`producer_and_client`], but with an explicit hash cache — `None`
    /// standing for a producer that cannot vouch for anything.
    async fn producer_with_hashes(
        root: &std::path::Path,
        hashes: Option<Arc<super::hash::HashCache>>,
    ) -> (
        fofoca::iroh::Endpoint,
        RemoteClient,
        tokio::task::JoinHandle<()>,
    ) {
        let (manifest, paths) = super::scan::scan(root).expect("scan");
        let tree = Arc::new(super::live::LiveTree::new(
            root.to_path_buf(),
            manifest,
            paths,
        ));
        let (endpoint, ticket, secret, _webrtc) = produce::bind(LookupOpts::loopback(), None)
            .await
            .expect("bind producer");
        let auth = ShareAuth::new(&secret, None);
        let accept_endpoint = endpoint.clone();
        let producer = tokio::spawn(async move {
            while let Some(incoming) = accept_endpoint.accept().await {
                let tree = Arc::clone(&tree);
                let hashes = hashes.clone();
                tokio::spawn(async move {
                    let Ok(conn) = incoming.await else { return };
                    let _ = produce::serve_established(conn, auth, tree, hashes).await;
                });
            }
        });

        let consumer_endpoint =
            build_endpoint(&ticket.lookups, None, None, Vec::new(), None, false)
                .await
                .expect("consumer endpoint");
        add_peer_addr(&consumer_endpoint, ticket.addr.clone()).expect("add peer addr");
        let client = RemoteClient::new(consumer_endpoint, ticket, auth);
        (endpoint, client, producer)
    }

    /// Stand up a producer serving `root` under a **caller-supplied** secret
    /// rather than the one `bind` mints for it, optionally behind `password`.
    ///
    /// This is the re-seeder shape exactly: a peer that holds the bytes serves
    /// them under the *origin's* ticket secret. `serve_established` already
    /// takes the credential as a parameter, so no production code moves to make
    /// this possible — which is the claim under test.
    async fn producer_under_secret(
        root: &std::path::Path,
        secret: [u8; SECRET_LEN],
        password: Option<&str>,
    ) -> (
        fofoca::iroh::Endpoint,
        MountTicket,
        tokio::task::JoinHandle<()>,
    ) {
        let (manifest, paths) = super::scan::scan(root).expect("scan");
        let tree = Arc::new(super::live::LiveTree::new(
            root.to_path_buf(),
            manifest,
            paths,
        ));
        let (endpoint, mut ticket, _minted, _webrtc) = produce::bind(LookupOpts::loopback(), None)
            .await
            .expect("bind re-seeder");
        // Advertise the origin's secret, not the freshly minted one.
        ticket.secret = secret;
        let auth = ShareAuth::new(&secret, password);
        if auth.password_protected() {
            ticket.flags |= agent_share_proto::ticket::TICKET_FLAG_PASSWORD;
        }
        let accept_endpoint = endpoint.clone();
        let task = tokio::spawn(async move {
            while let Some(incoming) = accept_endpoint.accept().await {
                let tree = Arc::clone(&tree);
                tokio::spawn(async move {
                    let Ok(conn) = incoming.await else { return };
                    let _ = produce::serve_established(conn, auth, tree, None).await;
                });
            }
        });
        (endpoint, ticket, task)
    }

    /// Build a client that talks to `ticket`'s address, presenting what
    /// `ticket` plus `password` derives.
    ///
    /// Goes through `redeem_auth` rather than deriving directly, so the tests
    /// exercise the same gate the CLI does.
    async fn client_for(ticket: MountTicket, password: Option<&str>) -> RemoteClient {
        let auth = super::consume::redeem_auth(&ticket, password)
            .expect("redeem")
            .auth;
        let endpoint = build_endpoint(&ticket.lookups, None, None, Vec::new(), None, false)
            .await
            .expect("client endpoint");
        add_peer_addr(&endpoint, ticket.addr.clone()).expect("add peer addr");
        RemoteClient::new(endpoint, ticket, auth)
    }

    /// As [`client_for`], but presenting a credential the ticket's own flag
    /// does not sanction — the shape of a wrong guess.
    async fn client_presenting(ticket: MountTicket, auth: ShareAuth) -> RemoteClient {
        let endpoint = build_endpoint(&ticket.lookups, None, None, Vec::new(), None, false)
            .await
            .expect("client endpoint");
        add_peer_addr(&endpoint, ticket.addr.clone()).expect("add peer addr");
        RemoteClient::new(endpoint, ticket, auth)
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
            producer_under_secret(&origin_tree.path, secret, None).await;

        // A second host with its own endpoint and its own copy of the bytes,
        // serving under the origin's secret. A mirror, in other words.
        let mirror_tree = fixture_tree();
        let (mirror_endpoint, mirror_ticket, mirror_task) =
            producer_under_secret(&mirror_tree.path, secret, None).await;
        assert_ne!(
            mirror_ticket.addr, origin_ticket.addr,
            "the mirror must be a genuinely different endpoint"
        );

        // A consumer pointed at the mirror, holding only the origin's ticket
        // secret, is served — no new code anywhere.
        let mirror_client = client_for(mirror_ticket, None).await;
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

    /// The password feature's central claim: the ticket addresses the share,
    /// the password opens it, and holding one without the other gets nothing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_protected_share_serves_the_password_and_refuses_the_bare_ticket() {
        let mut secret = [0u8; SECRET_LEN];
        rand::rng().fill_bytes(&mut secret);

        let tree = fixture_tree();
        let (endpoint, ticket, task) =
            producer_under_secret(&tree.path, secret, Some("hunter2")).await;
        assert!(
            ticket.password_protected(),
            "the ticket must advertise that it needs a password"
        );

        // The right password reads the tree.
        let client = client_for(ticket.clone(), Some("hunter2")).await;
        let manifest = client
            .fetch_manifest()
            .await
            .expect("the right password must open the share");
        let hello = manifest
            .files
            .iter()
            .position(|file| file.rel_path == "hello.txt")
            .expect("hello.txt listed");
        let bytes = client
            .read_range(u32::try_from(hello).expect("index"), 0, 5)
            .await
            .expect("read");
        assert_eq!(&bytes, b"hello");

        // The bare ticket secret — everything an eavesdropper on the link has —
        // does not. This is the case that would have worked before the feature.
        let bearer = client_presenting(ticket.clone(), ShareAuth::from_token(secret, false)).await;
        assert!(
            bearer.fetch_manifest().await.is_err(),
            "the ticket secret alone must not open a protected share"
        );

        // And neither does a wrong guess.
        let wrong = client_for(ticket, Some("hunter3")).await;
        assert!(wrong.fetch_manifest().await.is_err(), "wrong password");

        endpoint.close().await;
        task.abort();
    }

    /// A refused password is reported *as* a refused password.
    ///
    /// The producer's close code is the only signal — there is no verifier in
    /// the ticket, deliberately — so if this regresses the user gets a discovery
    /// timeout instead of "try again", with no way to tell the two apart.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_wrong_password_is_distinguishable_from_an_unreachable_share() {
        let mut secret = [0u8; SECRET_LEN];
        rand::rng().fill_bytes(&mut secret);

        let tree = fixture_tree();
        let (endpoint, ticket, task) =
            producer_under_secret(&tree.path, secret, Some("hunter2")).await;

        let wrong = client_for(ticket, Some("hunter3")).await;
        assert!(wrong.fetch_manifest().await.is_err());
        assert!(
            wrong.refused_for_password().await,
            "the producer's close must say the credential was refused, not just that it hung up"
        );

        endpoint.close().await;
        task.abort();
    }

    /// A share with no password is refused the old way, so the close code
    /// cannot be read as "there is a password here" on a share that has none.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unprotected_share_refuses_a_bad_bearer_without_claiming_a_password() {
        let mut secret = [0u8; SECRET_LEN];
        rand::rng().fill_bytes(&mut secret);

        let tree = fixture_tree();
        let (endpoint, ticket, task) = producer_under_secret(&tree.path, secret, None).await;

        let impostor =
            client_presenting(ticket, ShareAuth::from_token([0xAAu8; SECRET_LEN], true)).await;
        assert!(impostor.fetch_manifest().await.is_err());
        assert!(
            !impostor.refused_for_password().await,
            "an unprotected share must not invite a password retry"
        );

        endpoint.close().await;
        task.abort();
    }

    /// The gate `redeem_auth` puts in front of the dial: both mismatches fail
    /// before a packet moves, and the matching cases derive what the wire wants.
    #[test]
    fn redeeming_checks_the_password_against_the_ticket_flag() {
        let mut secret = [0u8; SECRET_LEN];
        rand::rng().fill_bytes(&mut secret);
        let plain = MountTicket {
            addr: fofoca::iroh::EndpointAddr::new(
                fofoca::iroh::SecretKey::from_bytes(&[7u8; 32]).public(),
            ),
            secret,
            lookups: LookupOpts::loopback(),
            kind: agent_share_proto::ticket::TICKET_KIND_SHARE,
            flags: 0,
            mesh_id: None,
        };
        let mut protected = plain.clone();
        protected.flags |= agent_share_proto::ticket::TICKET_FLAG_PASSWORD;

        assert!(
            super::consume::redeem_auth(&protected, None).is_err(),
            "a protected ticket with no password must fail before the dial"
        );
        assert!(
            super::consume::redeem_auth(&plain, Some("hunter2")).is_err(),
            "a password offered to an unprotected ticket means the wrong ticket"
        );
        assert_eq!(
            super::consume::redeem_auth(&plain, None)
                .expect("plain")
                .auth
                .token(),
            &secret,
            "an unprotected redeem presents the ticket secret verbatim"
        );
        assert_eq!(
            super::consume::redeem_auth(&protected, Some("hunter2"))
                .expect("protected")
                .auth
                .token(),
            &share_token(&secret, Some("hunter2")),
        );
    }

    /// **The whole point of the mesh id in the ticket.**
    ///
    /// A wrong password is named with no producer, no seeder and no socket —
    /// `fofoca` decodes the id, stretches the password, and compares it against
    /// the verifier the id carries. Nothing here binds an endpoint, which is the
    /// assertion: a share is designed to outlive its producer, so a check that
    /// needs one is a check that usually cannot run.
    #[test]
    fn a_wrong_password_is_named_without_any_network() {
        let secret = [21u8; SECRET_LEN];
        let lookups = LookupOpts::loopback();

        let minted = super::mesh::mint(&secret, &lookups, Some("hunter2")).expect("mint");
        let mesh_id = minted.mesh_id().to_owned();

        let wrong = super::mesh::resolve(Some(&mesh_id), &secret, &lookups, Some("hunter3"))
            .expect_err("a wrong password must be refused");
        assert!(
            format!("{wrong:#}").contains(super::consume::WRONG_PASSWORD),
            "the refusal must name the password: {wrong:#}"
        );

        // And the right one resolves onto the same mesh the producer minted.
        let right = super::mesh::resolve(Some(&mesh_id), &secret, &lookups, Some("hunter2"))
            .expect("the right password must resolve");
        assert_eq!(right.mesh_id(), mesh_id);
    }

    /// A protected ticket that carries no mesh id — one an older producer
    /// minted — still resolves. It simply has nothing to check against, and
    /// falls back to the producer refusing the token on the wire.
    #[test]
    fn a_protected_ticket_without_a_mesh_id_still_resolves() {
        let secret = [22u8; SECRET_LEN];
        let lookups = LookupOpts::loopback();
        assert!(super::mesh::resolve(None, &secret, &lookups, Some("anything")).is_ok());
    }

    /// Two passwords, two meshes. A holder of the link and the wrong password
    /// does not merely fail to read — they never find the peers either, so a
    /// producer-less share stays invisible to them.
    #[test]
    fn a_wrong_password_lands_on_a_different_mesh() {
        let secret = [23u8; SECRET_LEN];
        let lookups = LookupOpts::loopback();
        let one = super::mesh::mint(&secret, &lookups, Some("hunter2")).expect("mint");
        let two = super::mesh::mint(&secret, &lookups, Some("hunter3")).expect("mint");
        assert_ne!(one.mesh_id(), two.mesh_id());
        // And neither is the unprotected share's mesh.
        let plain = super::mesh::mint(&secret, &lookups, None).expect("mint");
        assert_ne!(one.mesh_id(), plain.mesh_id());
    }

    /// The mesh moves with the password too, which is what makes the gate a
    /// gate rather than a read check: a link-holder without the password lands
    /// on a different mesh id, so they never see the peers, the tree
    /// fingerprint or the serving grid either.
    #[test]
    fn a_protected_share_derives_a_mesh_nobody_with_just_the_link_can_find() {
        use agent_share_proto::mesh_key::share_mesh_key;

        let secret = [3u8; SECRET_LEN];
        let bearer_only = share_mesh_key(&secret);
        let with_password = share_mesh_key(&share_token(&secret, Some("hunter2")));
        assert_ne!(bearer_only, with_password);
        assert_ne!(
            with_password,
            share_mesh_key(&share_token(&secret, Some("hunter3"))),
        );
        // And an unprotected share is exactly where it always was.
        assert_eq!(bearer_only, share_mesh_key(&share_token(&secret, None)));
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
            producer_under_secret(&origin_tree.path, secret, None).await;
        let origin_client = client_for(origin_ticket, None).await;

        // Same shape, different contents — a stale mirror.
        let stale = TempDir::new();
        std::fs::create_dir_all(stale.path.join("docs")).unwrap();
        std::fs::write(stale.path.join("hello.txt"), b"WRONG WORLD").unwrap();
        std::fs::write(stale.path.join("docs/guide.md"), b"stale bytes").unwrap();

        let (stale_endpoint, stale_ticket, stale_task) =
            producer_under_secret(&stale.path, secret, None).await;
        let stale_client = client_for(stale_ticket, None).await;

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
            flags: 0,
            mesh_id: None,
        };
        let bad_endpoint = build_endpoint(&bad_ticket.lookups, None, None, Vec::new(), None, false)
            .await
            .expect("bad-client endpoint");
        add_peer_addr(&bad_endpoint, bad_ticket.addr.clone()).expect("add peer addr");
        let bad_auth = ShareAuth::new(&bad_ticket.secret, None);
        let bad = RemoteClient::new(bad_endpoint, bad_ticket, bad_auth);
        assert!(bad.fetch_manifest().await.is_err(), "bad secret must fail");

        // The honest client still works — the producer keeps accepting.
        assert!(client.fetch_manifest().await.is_ok());

        endpoint.close().await;
        producer.abort();
    }
}
