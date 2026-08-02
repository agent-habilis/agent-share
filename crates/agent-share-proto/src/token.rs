//! The token codec shared by every agent-habilis token — here the mount
//! ticket ([`crate::ticket`]); in agent-habilis/swarm also the swarm id and
//! the pipe/port/file/sh tickets. One wire shape for every token, so a token
//! string self-describes its kind via a 1-byte type tag and the namespaces
//! never collide. The full [`TokenType`] enum is kept (not trimmed to
//! `Mount`) as wire documentation, and so a non-mount token decodes to a
//! clean "wrong token type" error rather than an "unknown type" one.
//!
//! Wire: Base58Check(`version ‖ type ‖ payload`) with a `SHA256d` checksum.
//! Unprefixed and entirely ASCII, so a token drops into a URL path segment
//! or a file path verbatim — no percent-encoding, no escaping.

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

/// Token framing version. Bumped only on a breaking framing change; an
/// unknown version is rejected on decode.
const VERSION: u8 = 1;

/// Which kind of token this is — the byte that lets one token namespace
/// carry both swarm ids and pipe tickets without ambiguity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenType {
    Swarm,
    Pipe,
    Port,
    File,
    Mount,
    Sh,
}

impl TokenType {
    fn to_byte(self) -> u8 {
        match self {
            TokenType::Swarm => 1,
            TokenType::Pipe => 2,
            TokenType::Port => 3,
            TokenType::File => 4,
            // 5 shipped on main as the mount ticket while sh was still local;
            // sh takes 6 — the type byte is wire format and never reassigned.
            TokenType::Mount => 5,
            TokenType::Sh => 6,
        }
    }

    fn from_byte(byte: u8) -> Result<Self> {
        match byte {
            1 => Ok(TokenType::Swarm),
            2 => Ok(TokenType::Pipe),
            3 => Ok(TokenType::Port),
            4 => Ok(TokenType::File),
            5 => Ok(TokenType::Mount),
            6 => Ok(TokenType::Sh),
            other => bail!("unknown token type: {other}"),
        }
    }
}

/// Encode `payload` as a token of the given `kind`.
#[must_use]
pub fn encode(kind: TokenType, payload: &[u8]) -> String {
    let mut framed = Vec::with_capacity(2 + payload.len());
    framed.push(VERSION);
    framed.push(kind.to_byte());
    framed.extend_from_slice(payload);
    base58check_encode(&framed)
}

/// Decode a token into its kind and raw payload, validating the Base58Check
/// checksum and the version byte.
///
/// # Errors
/// Invalid Base58, a bad checksum, an unknown version, or an unknown type
/// byte.
pub fn decode(token: &str) -> Result<(TokenType, Vec<u8>)> {
    let framed = base58check_decode(token)?;
    let version = *framed.first().context("token too short")?;
    if version != VERSION {
        bail!("unsupported token version: {version}");
    }
    let kind = TokenType::from_byte(*framed.get(1).context("token too short")?)?;
    Ok((kind, framed[2..].to_vec()))
}

fn checksum(bytes: &[u8]) -> [u8; 4] {
    let first = Sha256::digest(bytes);
    let second = Sha256::digest(first);
    let mut out = [0u8; 4];
    out.copy_from_slice(&second[..4]);
    out
}

fn base58check_encode(payload: &[u8]) -> String {
    let mut with_checksum = payload.to_vec();
    with_checksum.extend_from_slice(&checksum(payload));
    bs58::encode(with_checksum).into_string()
}

fn base58check_decode(encoded: &str) -> Result<Vec<u8>> {
    let decoded = bs58::decode(encoded)
        .into_vec()
        .context("invalid Base58 token encoding")?;
    if decoded.len() < 4 {
        bail!("token too short");
    }
    let (payload, received) = decoded.split_at(decoded.len() - 4);
    if received != checksum(payload) {
        bail!("invalid token checksum");
    }
    Ok(payload.to_vec())
}

#[cfg(test)]
mod tests {
    use super::{TokenType, VERSION, base58check_encode, decode, encode};

    #[test]
    fn round_trips_each_kind() {
        for kind in [
            TokenType::Swarm,
            TokenType::Pipe,
            TokenType::Port,
            TokenType::File,
            TokenType::Mount,
            TokenType::Sh,
        ] {
            let token = encode(kind, b"payload-bytes");
            // A token is unprefixed ASCII Base58, so it needs no escaping in
            // a URL path segment or a filename.
            assert!(
                token.bytes().all(|byte| byte.is_ascii_alphanumeric()),
                "token must be ASCII Base58: {token}"
            );
            let (decoded_kind, payload) = decode(&token).expect("decode");
            assert_eq!(decoded_kind, kind);
            assert_eq!(payload, b"payload-bytes");
        }
    }

    #[test]
    fn type_bytes_are_pinned_wire_format() {
        // Interop tripwire: these bytes are shared with agent-habilis/swarm's
        // `ahsw` and must never drift (a mount ticket is type 5 on both ends).
        for (kind, byte) in [
            (TokenType::Swarm, 1u8),
            (TokenType::Pipe, 2),
            (TokenType::Port, 3),
            (TokenType::File, 4),
            (TokenType::Mount, 5),
            (TokenType::Sh, 6),
        ] {
            assert_eq!(kind.to_byte(), byte);
            assert_eq!(TokenType::from_byte(byte).unwrap(), kind);
        }
    }

    #[test]
    fn rejects_bad_checksum() {
        let mut token = encode(TokenType::Swarm, b"payload");
        let last = token.pop().unwrap();
        token.push(if last == '1' { '2' } else { '1' });
        assert!(decode(&token).is_err());
    }

    #[test]
    fn rejects_unknown_version() {
        let token = base58check_encode(&[9u8, 1u8, 0u8]);
        assert!(decode(&token).is_err());
    }

    #[test]
    fn rejects_unknown_type() {
        let token = base58check_encode(&[VERSION, 9u8, 0u8]);
        assert!(decode(&token).is_err());
    }
}
