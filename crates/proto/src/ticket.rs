//! The mount ticket — the whole capability to read a share, in one string.

use anyhow::{Context, Result, bail};
use iroh_base::EndpointAddr;

use crate::framing::SECRET_LEN;
use crate::lookup::LookupOpts;
use crate::peer_addr::{endpoint_addr_from_json, endpoint_addr_to_json};
use crate::token::{self, TokenType};

/// A decoded mount ticket — the bearer secret, the share's discovery config,
/// and the producer's address. Payload layout mirrors the file ticket:
/// `secret(32) ‖ flags(1) ‖ lookups ‖ address-json` (lookups is
/// self-delimiting, so the address occupies the remainder). `flags` is
/// reserved for forward-compat and always 0 today.
///
/// The secret is a pure bearer capability: whoever holds this string can read
/// the share. That is why the web client keeps it in the URL *fragment*,
/// which is never sent to a server.
#[derive(Debug, Clone)]
pub struct MountTicket {
    pub addr: EndpointAddr,
    pub secret: [u8; SECRET_LEN],
    pub lookups: LookupOpts,
}

impl MountTicket {
    /// Encode as a `🐝` token (`type = mount`).
    ///
    /// # Panics
    /// If the embedded [`LookupOpts`] exceeds its wire bounds — see
    /// [`LookupOpts::encode_into`]. The address JSON cannot fail.
    #[must_use]
    pub fn encode(&self) -> String {
        let mut payload = Vec::with_capacity(SECRET_LEN + 1 + 64);
        payload.extend_from_slice(&self.secret);
        payload.push(0); // reserved flags byte
        self.lookups.encode_into(&mut payload);
        let addr_json = serde_json::to_vec(&endpoint_addr_to_json(&self.addr))
            .expect("EndpointAddr JSON always serializes");
        payload.extend_from_slice(&addr_json);
        token::encode(TokenType::Mount, &payload)
    }

    /// Decode a `🐝` mount ticket.
    ///
    /// # Errors
    /// Not a `🐝` token, the wrong token type, or a malformed payload.
    pub fn decode(ticket: &str) -> Result<Self> {
        let (kind, payload) = token::decode(ticket.trim())?;
        if kind != TokenType::Mount {
            bail!("not a mount ticket: wrong token type");
        }
        let secret_slice = payload.get(..SECRET_LEN).context("ticket too short")?;
        let mut secret = [0u8; SECRET_LEN];
        secret.copy_from_slice(secret_slice);
        // Skip the reserved flags byte.
        let mut pos = SECRET_LEN + 1;
        if payload.len() < pos {
            bail!("ticket missing flags");
        }
        let lookups = LookupOpts::decode_from(&payload, &mut pos)?;
        let addr_json = payload.get(pos..).context("ticket missing address")?;
        let value: serde_json::Value =
            serde_json::from_slice(addr_json).context("invalid ticket address json")?;
        let (_id, addr) = endpoint_addr_from_json(&value)?;
        Ok(Self {
            addr,
            secret,
            lookups,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{MountTicket, SECRET_LEN};
    use crate::lookup::LookupOpts;
    use crate::token::{self, TokenType};
    use iroh_base::{EndpointAddr, SecretKey};

    fn sample() -> MountTicket {
        let id = SecretKey::from_bytes(&[7u8; 32]).public();
        MountTicket {
            addr: EndpointAddr::new(id).with_ip_addr("127.0.0.1:4242".parse().expect("addr")),
            secret: [5u8; SECRET_LEN],
            lookups: LookupOpts::public_preset(),
        }
    }

    #[test]
    fn ticket_round_trips() {
        let ticket = sample();
        let encoded = ticket.encode();
        assert!(encoded.starts_with("🐝"));
        let decoded = MountTicket::decode(&encoded).expect("decode");
        assert_eq!(decoded.addr.id, ticket.addr.id);
        assert_eq!(decoded.secret, ticket.secret);
        assert_eq!(decoded.lookups, ticket.lookups);
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
        // A `🐝` token of the wrong kind is valid framing but must not decode
        // as a mount ticket — that is what the type byte is for.
        let swarm = token::encode(TokenType::Swarm, &[0u8; 64]);
        assert!(MountTicket::decode(&swarm).is_err());
    }

    #[test]
    fn rejects_a_truncated_payload() {
        let mount = token::encode(TokenType::Mount, &[0u8; SECRET_LEN - 1]);
        assert!(MountTicket::decode(&mount).is_err());
    }
}
