//! The mount ticket — the whole capability to read a share, in one string.

use anyhow::{Context, Result, bail};
use fofoca_protocol::iroh_base::EndpointAddr;

use crate::framing::SECRET_LEN;
use crate::lookup::LookupOpts;
use crate::peer_addr::{endpoint_addr_from_json, endpoint_addr_to_json};
use crate::token::{self, TokenType};

// Ticket *kind* — one byte, matched by exact value. These are mutually
// exclusive alternatives, not bits: `2` is the relay bench, not "webrtc plus
// something". They were once named `TICKET_FLAG_*`, which invited
// `kind & TICKET_KIND_BENCH_WEBRTC` — a test that reads true for the relay
// bench and false for an ordinary share, silently.

/// An ordinary file share.
pub const TICKET_KIND_SHARE: u8 = 0;

/// Bench producer chose `WebRTC` for the mount data path.
pub const TICKET_KIND_BENCH_WEBRTC: u8 = 1;

/// Bench producer chose the iroh relay / ticket address for the mount data path.
pub const TICKET_KIND_BENCH_RELAY: u8 = 2;

/// Bench producer chose plain iroh `QUIC` over UDP — no `WebRTC` wrapper, no
/// forced relay. The control leg the other two are measured against.
pub const TICKET_KIND_BENCH_QUIC: u8 = 3;

/// A decoded mount ticket — the bearer secret, the share's discovery config,
/// and the producer's address.
///
/// Payload layout: `secret(32) ‖ kind(1) ‖ lookups ‖ addr_len(u16) ‖ addr_json`.
/// Every field is self-delimiting, **including the last one**, which is the
/// point: the address used to run to the end of the payload, so nothing could
/// ever be appended after it and any new field cost a format break. It no
/// longer does.
///
/// `kind` is [`TICKET_KIND_SHARE`] for ordinary shares. Bench tickets set
/// [`TICKET_KIND_BENCH_WEBRTC`], [`TICKET_KIND_BENCH_RELAY`] or
/// [`TICKET_KIND_BENCH_QUIC`] so the consumer knows which path the producer
/// opened. Values are dense rather than bit flags, so a peer built before a
/// value existed rejects it outright instead of misreading it.
///
/// The token envelope's `VERSION` is deliberately **not** bumped for this
/// layout change. That byte is shared by every [`TokenType`] — swarm ids, pipe,
/// port, file and sh tokens — so bumping it to describe a mount-only change
/// would retire all of them. A ticket in the old layout fails to decode with a
/// parse error rather than a version error, which is acceptable here because a
/// mount ticket is **already ephemeral**: it embeds the producer's live
/// `EndpointAddr`, so it dies when that producer restarts, format change or no.
///
/// The secret is a pure bearer capability: whoever holds this string can read
/// the share. The web client puts it in the path (`/files/<ticket>`,
/// `/info/<ticket>`) so those views are shareable as ordinary URLs.
#[derive(Debug, Clone)]
pub struct MountTicket {
    pub addr: EndpointAddr,
    pub secret: [u8; SECRET_LEN],
    pub lookups: LookupOpts,
    pub kind: u8,
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
        payload.push(self.kind);
        self.lookups.encode_into(&mut payload);
        let addr_json = serde_json::to_vec(&endpoint_addr_to_json(&self.addr))
            .expect("EndpointAddr JSON always serializes");
        payload.extend_from_slice(
            &u16::try_from(addr_json.len())
                .expect("an EndpointAddr JSON is orders of magnitude under 64 KiB")
                .to_le_bytes(),
        );
        payload.extend_from_slice(&addr_json);
        token::encode(TokenType::Mount, &payload)
    }

    /// Decode a mount ticket.
    ///
    /// # Errors
    /// Not a valid token, the wrong token type, or a malformed payload.
    pub fn decode(ticket: &str) -> Result<Self> {
        let (token_type, payload) = token::decode(ticket.trim())?;
        if token_type != TokenType::Mount {
            bail!("not a mount ticket: wrong token type");
        }
        let secret_slice = payload.get(..SECRET_LEN).context("ticket too short")?;
        let mut secret = [0u8; SECRET_LEN];
        secret.copy_from_slice(secret_slice);
        let mut pos = SECRET_LEN;
        let kind = *payload.get(pos).context("ticket missing kind")?;
        pos += 1;
        let lookups = LookupOpts::decode_from(&payload, &mut pos)?;
        let len_bytes = payload
            .get(pos..pos + 2)
            .context("ticket missing address length")?;
        let addr_len = usize::from(u16::from_le_bytes([len_bytes[0], len_bytes[1]]));
        pos += 2;
        let addr_json = payload
            .get(pos..pos + addr_len)
            .context("ticket address truncated")?;
        pos += addr_len;
        // Trailing bytes are tolerated, not rejected: that is what makes the
        // layout extensible. A later field appended here is ignored by this
        // build rather than failing it.
        let _ = pos;
        let value: serde_json::Value =
            serde_json::from_slice(addr_json).context("invalid ticket address json")?;
        let (_id, addr) = endpoint_addr_from_json(&value)?;
        Ok(Self {
            addr,
            secret,
            lookups,
            kind,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MountTicket, SECRET_LEN, TICKET_KIND_BENCH_QUIC, TICKET_KIND_BENCH_RELAY,
        TICKET_KIND_BENCH_WEBRTC, TICKET_KIND_SHARE,
    };
    use crate::lookup::LookupOpts;
    use crate::peer_addr::endpoint_addr_to_json;
    use crate::token::{self, TokenType};
    use fofoca_protocol::iroh_base::{EndpointAddr, SecretKey};

    fn sample() -> MountTicket {
        let id = SecretKey::from_bytes(&[7u8; 32]).public();
        MountTicket {
            addr: EndpointAddr::new(id).with_ip_addr("127.0.0.1:4242".parse().expect("addr")),
            secret: [5u8; SECRET_LEN],
            lookups: LookupOpts::public_preset(),
            kind: TICKET_KIND_SHARE,
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
        assert_eq!(decoded.kind, TICKET_KIND_SHARE);
    }

    #[test]
    fn bench_kinds_round_trip() {
        for kind in [
            TICKET_KIND_BENCH_WEBRTC,
            TICKET_KIND_BENCH_RELAY,
            TICKET_KIND_BENCH_QUIC,
        ] {
            let mut ticket = sample();
            ticket.kind = kind;
            let decoded = MountTicket::decode(&ticket.encode()).expect("decode");
            assert_eq!(decoded.kind, kind);
        }
    }

    /// **The reason the layout changed.** The address used to run to the end of
    /// the payload, so no field could ever follow it. Now it is length-prefixed,
    /// and bytes appended after it are ignored rather than fatal — which is what
    /// lets a later field land without another format break.
    #[test]
    fn a_field_appended_after_the_address_is_ignored() {
        let ticket = sample();
        let (kind, mut payload) = token::decode(&ticket.encode()).expect("decode token");
        payload.extend_from_slice(b"a future field this build knows nothing about");
        let extended = token::encode(kind, &payload);

        let decoded = MountTicket::decode(&extended).expect("trailing bytes must not be fatal");
        assert_eq!(decoded.addr.id, ticket.addr.id);
        assert_eq!(decoded.secret, ticket.secret);
    }

    /// A ticket in the pre-length-prefix layout must fail, not silently
    /// misparse into a plausible-looking address.
    #[test]
    fn a_ticket_in_the_retired_layout_is_rejected() {
        let ticket = sample();
        // The old payload: secret ‖ kind ‖ lookups ‖ addr-json, no length.
        let mut payload = Vec::new();
        payload.extend_from_slice(&ticket.secret);
        payload.push(ticket.kind);
        ticket.lookups.encode_into(&mut payload);
        payload.extend_from_slice(
            &serde_json::to_vec(&endpoint_addr_to_json(&ticket.addr)).expect("json"),
        );
        let old = token::encode(TokenType::Mount, &payload);

        assert!(
            MountTicket::decode(&old).is_err(),
            "a retired-layout ticket must be rejected outright"
        );
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
