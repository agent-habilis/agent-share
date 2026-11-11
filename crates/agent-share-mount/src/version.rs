//! Telling a peer on another version of the mount protocol apart from a peer
//! that cannot be reached.
//!
//! [`agent_share_proto::MOUNT_ALPN`] carries the protocol version, and QUIC
//! refuses a handshake whose ALPN the other side does not speak. Both clients
//! dial it, and both have to say the same thing when that happens, so the
//! check and the words live here once.

use habilis_network::iroh::endpoint::{
    ConnectError, ConnectingError, ConnectionError, TransportErrorCode,
};

/// Why a producer refused the mount protocol at the handshake, in the user's
/// terms rather than as TLS alert 120.
///
/// The only reason it does is a version of `MOUNT_ALPN` it does not speak: the
/// two sides were built on different engines, and their chunk roots do not
/// agree.
pub const OTHER_VERSION: &str = "this share was made by a different version of agent-share, \
     and the two cannot read each other's files; update agent-share on both machines (the web \
     app at agent-share.dev is always the latest)";

/// TLS alert 120, `no_application_protocol`: the peer speaks none of the
/// ALPNs this side offered.
const NO_APPLICATION_PROTOCOL: u8 = 120;

/// Whether `error` is the peer refusing our `MOUNT_ALPN` at the handshake.
///
/// Permanent, unlike the other dial failures: redialing a peer that speaks
/// another version of the protocol fails the same way every time, so a caller
/// stops retrying on it. Matched on the QUIC close code rather than on the
/// rendered message, which is the TLS library's wording to change.
#[must_use]
pub fn refused_protocol(error: &ConnectError) -> bool {
    let connection_error = if let ConnectError::Connection { source, .. } = error {
        source
    } else if let ConnectError::Connecting {
        source: ConnectingError::ConnectionError { source, .. },
        ..
    } = error
    {
        source
    } else {
        return false;
    };
    matches!(
        connection_error,
        ConnectionError::ConnectionClosed(close)
            if close.error_code == TransportErrorCode::crypto(NO_APPLICATION_PROTOCOL)
    )
}
