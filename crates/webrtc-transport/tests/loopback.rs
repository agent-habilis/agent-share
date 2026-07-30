use std::time::Duration;

use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh::{Endpoint, EndpointAddr, SecretKey, TransportAddr};
use webrtc_transport::{
    IceConfig, WEBRTC_TRANSPORT_ID, WebRtcTransport, answer_with, custom_addr, offer_with,
};

const ECHO_ALPN: &[u8] = b"agent-gossip-webrtc/test-echo/0";
const JSEP_DEADLINE: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
struct Echo;

impl ProtocolHandler for Echo {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let (mut send, mut recv) = connection.accept_bi().await?;
        tokio::io::copy(&mut recv, &mut send).await?;
        send.finish()?;
        connection.closed().await;
        Ok(())
    }
}

#[tokio::test]
async fn quic_echo_over_webrtc() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
    let key_client = SecretKey::generate();
    let key_server = SecretKey::generate();
    let id_client = key_client.public();
    let id_server = key_server.public();

    let transport_client = WebRtcTransport::new(id_client);
    let transport_server = WebRtcTransport::new(id_server);

    // JSEP with the envelopes handed over in memory (the carrier is the
    // caller's concern by design).
    let (pending_offer, offer_env) = offer_with(id_client, &IceConfig::host_only()).await?;
    let (pending_answer, answer_env) =
        answer_with(id_server, &offer_env, &IceConfig::host_only()).await?;
    assert_eq!(offer_env.claimed_endpoint()?, id_client);
    assert_eq!(answer_env.claimed_endpoint()?, id_server);
    let (client_session, server_session) = tokio::join!(
        pending_offer.complete(&answer_env, JSEP_DEADLINE),
        pending_answer.complete(JSEP_DEADLINE),
    );
    transport_client.attach(id_server, client_session?)?;
    transport_server.attach(id_client, server_session?)?;
    assert!(transport_client.has_session(&id_server));
    assert!(transport_server.has_session(&id_client));

    let endpoint_server = Endpoint::builder(transport_server.preset())
        .secret_key(key_server)
        .bind()
        .await?;
    let router = Router::builder(endpoint_server)
        .accept(ECHO_ALPN, Echo)
        .spawn();

    let endpoint_client = Endpoint::builder(transport_client.preset())
        .secret_key(key_client)
        .bind()
        .await?;

    let server_addr =
        EndpointAddr::from_parts(id_server, [TransportAddr::Custom(custom_addr(id_server))]);
    let connection = endpoint_client.connect(server_addr, ECHO_ALPN).await?;

    // The only transport is WebRTC, but assert the selected path anyway so
    // a future regression can't silently reroute.
    let paths = connection.paths();
    let on_webrtc = paths.iter().any(|path| {
        matches!(
            path.remote_addr(),
            TransportAddr::Custom(addr) if addr.id() == WEBRTC_TRANSPORT_ID
        )
    });
    assert!(on_webrtc, "no WebRTC path on the connection: {paths:?}");

    let (mut send, mut recv) = connection.open_bi().await?;
    send.write_all(b"hello over webrtc").await?;
    send.finish()?;
    let echoed = recv.read_to_end(64).await?;
    assert_eq!(echoed, b"hello over webrtc");

    connection.close(0u32.into(), b"done");
    router.shutdown().await?;
    endpoint_client.close().await;

    Ok(())
}

#[tokio::test]
async fn detach_then_reattach() -> anyhow::Result<()> {
    let key_client = SecretKey::generate();
    let key_server = SecretKey::generate();
    let id_client = key_client.public();
    let id_server = key_server.public();
    let transport_client = WebRtcTransport::new(id_client);

    let (pending_offer, offer_env) = offer_with(id_client, &IceConfig::host_only()).await?;
    let (pending_answer, answer_env) =
        answer_with(id_server, &offer_env, &IceConfig::host_only()).await?;
    let (client_session, server_session) = tokio::join!(
        pending_offer.complete(&answer_env, JSEP_DEADLINE),
        pending_answer.complete(JSEP_DEADLINE),
    );
    drop(server_session?);

    transport_client.attach(id_server, client_session?)?;
    assert!(transport_client.has_session(&id_server));

    // Second attach while live must fail.
    let (second_offer, second_env) = offer_with(id_client, &IceConfig::host_only()).await?;
    let (second_pending_answer, second_answer_env) =
        answer_with(id_server, &second_env, &IceConfig::host_only()).await?;
    let (second_client, second_server) = tokio::join!(
        second_offer.complete(&second_answer_env, JSEP_DEADLINE),
        second_pending_answer.complete(JSEP_DEADLINE),
    );
    drop(second_server?);
    let second_session = second_client?;
    assert!(transport_client.attach(id_server, second_session).is_err());

    assert!(transport_client.detach(&id_server));
    assert!(!transport_client.has_session(&id_server));
    assert_eq!(transport_client.session_count(), 0);
    Ok(())
}
