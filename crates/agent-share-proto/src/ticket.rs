//! The mount ticket — the whole capability to read a share, in one string.

use anyhow::{Context, Result, bail};
use iroh_base::EndpointAddr;

use crate::framing::SECRET_LEN;
use crate::lookup::LookupOpts;
use crate::peer_addr::{endpoint_addr_from_json, endpoint_addr_to_json};
use crate::token::{self, TokenType};

/// No special flags (ordinary file share).
pub const TICKET_FLAG_NONE: u8 = 0;

/// Bench producer chose `WebRTC` for the mount data path.
pub const TICKET_FLAG_BENCH_WEBRTC: u8 = 1;

/// Bench producer chose the iroh relay / ticket address for the mount data path.
pub const TICKET_FLAG_BENCH_RELAY: u8 = 2;

/// A decoded mount ticket — the bearer secret, the share's discovery config,
/// and the producer's address. Payload layout mirrors the file ticket:
/// `secret(32) ‖ flags(1) ‖ lookups ‖ address-json` (lookups is
/// self-delimiting, so the address occupies the remainder).
///
/// `flags` is `0` for ordinary shares. Bench tickets set
/// [`TICKET_FLAG_BENCH_WEBRTC`] or [`TICKET_FLAG_BENCH_RELAY`] so the consumer
/// knows which path the producer opened.
///
/// The secret is a pure bearer capability: whoever holds this string can read
/// the share. The web client puts it in the path (`/files/<ticket>`,
/// `/info/<ticket>`) so those views are shareable as ordinary URLs.
#[derive(Debug, Clone)]
pub struct MountTicket {
    pub addr: EndpointAddr,
    pub secret: [u8; SECRET_LEN],
    pub lookups: LookupOpts,
    pub flags: u8,
}

impl MountTicket {
    /// Encode as a token (`type = mount`).
    ///
    /// # Panics
    /// If the embedded [`LookupOpts`] exceeds its wire bounds — see
    /// [`LookupOpts::encode_into`]. The address JSON cannot fail.
    #[must_use]
    pub fn encode(&self) -> String {
        let mut payload = Vec::with_capacity(SECRET_LEN + 1 + 64);
        payload.extend_from_slice(&self.secret);
        payload.push(self.flags);
        self.lookups.encode_into(&mut payload);
        let addr_json = serde_json::to_vec(&endpoint_addr_to_json(&self.addr))
            .expect("EndpointAddr JSON always serializes");
        payload.extend_from_slice(&addr_json);
        token::encode(TokenType::Mount, &payload)
    }

    /// Decode a mount ticket.
    ///
    /// # Errors
    /// Not a valid token, the wrong token type, or a malformed payload.
    pub fn decode(ticket: &str) -> Result<Self> {
        let (kind, payload) = token::decode(ticket.trim())?;
        if kind != TokenType::Mount {
            bail!("not a mount ticket: wrong token type");
        }
        let secret_slice = payload.get(..SECRET_LEN).context("ticket too short")?;
        let mut secret = [0u8; SECRET_LEN];
        secret.copy_from_slice(secret_slice);
        let mut pos = SECRET_LEN;
        let flags = *payload.get(pos).context("ticket missing flags")?;
        pos += 1;
        let lookups = LookupOpts::decode_from(&payload, &mut pos)?;
        let addr_json = payload.get(pos..).context("ticket missing address")?;
        let value: serde_json::Value =
            serde_json::from_slice(addr_json).context("invalid ticket address json")?;
        let (_id, addr) = endpoint_addr_from_json(&value)?;
        Ok(Self {
            addr,
            secret,
            lookups,
            flags,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MountTicket, SECRET_LEN, TICKET_FLAG_BENCH_RELAY, TICKET_FLAG_BENCH_WEBRTC,
        TICKET_FLAG_NONE,
    };
    use crate::lookup::LookupOpts;
    use crate::token::{self, TokenType};
    use iroh_base::{EndpointAddr, SecretKey};

    fn sample() -> MountTicket {
        let id = SecretKey::from_bytes(&[7u8; 32]).public();
        MountTicket {
            addr: EndpointAddr::new(id).with_ip_addr("127.0.0.1:4242".parse().expect("addr")),
            secret: [5u8; SECRET_LEN],
            lookups: LookupOpts::public_preset(),
            flags: TICKET_FLAG_NONE,
        }
    }

    #[test]
    fn ticket_round_trips() {
        let ticket = sample();
        let encoded = ticket.encode();
        assert!(
            encoded.bytes().all(|byte| byte.is_ascii_alphanumeric()),
            "ticket must be ASCII Base58: {encoded}"
        );
        let decoded = MountTicket::decode(&encoded).expect("decode");
        assert_eq!(decoded.addr.id, ticket.addr.id);
        assert_eq!(decoded.secret, ticket.secret);
        assert_eq!(decoded.lookups, ticket.lookups);
        assert_eq!(decoded.flags, TICKET_FLAG_NONE);
    }

    #[test]
    fn bench_flags_round_trip() {
        for flags in [TICKET_FLAG_BENCH_WEBRTC, TICKET_FLAG_BENCH_RELAY] {
            let mut ticket = sample();
            ticket.flags = flags;
            let decoded = MountTicket::decode(&ticket.encode()).expect("decode");
            assert_eq!(decoded.flags, flags);
        }
    }

    #[test]
    fn surrounding_whitespace_is_tolerated() {
        // Tickets get copy-pasted out of terminals and URLs; a stray newline
        // must not be the difference between mounting and a cryptic error.
        let encoded = sample().encode();
        let padded = format!("  {encoded}\n");
        assert!(MountTicket::decode(&padded).is_ok());
    }

    #[test]
    fn rejects_another_token_type() {
        // A token of the wrong kind is valid framing but must not decode
        // as a mount ticket — that is what the type byte is for.
        let swarm = token::encode(TokenType::Swarm, &[0u8; 64]);
        assert!(MountTicket::decode(&swarm).is_err());
    }

    #[test]
    fn rejects_a_truncated_payload() {
        let short = token::encode(TokenType::Mount, &[0u8; 8]);
        assert!(MountTicket::decode(&short).is_err());
    }
}
