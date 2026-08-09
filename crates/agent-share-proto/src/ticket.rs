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

// Ticket *flags* — one byte, read as bits, and the opposite axis from `kind`.
// A share is one kind, but it can be any combination of the properties below,
// which is exactly why these could not have been more `kind` values.

/// The share is password-protected: the ticket's secret is only one of two
/// factors, and the bytes that actually open the mount are
/// [`crate::auth::share_token`] of that secret and the password.
///
/// Advisory, not enforcement — a consumer that ignores this bit simply presents
/// the raw secret and is refused. It exists so the consumer can *ask* for the
/// password up front instead of discovering the need from a dropped connection.
pub const TICKET_FLAG_PASSWORD: u8 = 0b0001;

/// The share carries an authorship key, and its manifests are signed.
///
/// Set independently of [`TICKET_FLAG_PASSWORD`] — a share may be signed,
/// protected, both, or neither.
pub const TICKET_FLAG_SIGNED: u8 = 0b0010;

/// A decoded mount ticket — the bearer secret, the share's discovery config,
/// and the producer's address.
///
/// Payload layout:
/// `secret(32) ‖ kind(1) ‖ lookups ‖ addr_len(u16) ‖ addr_json ‖ flags(1)?`.
/// Every field is self-delimiting, **including the address**, which is the
/// point: it used to run to the end of the payload, so nothing could ever be
/// appended after it and any new field cost a format break. It no longer does —
/// `flags` is the first field to land in the room that made.
///
/// `flags` is written only when non-zero, so an ordinary share's ticket stays
/// byte-identical to the one this crate emitted before the field existed. A
/// build that predates it ignores the trailing byte (see
/// [`MountTicket::decode`]) rather than failing.
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
/// Without [`TICKET_FLAG_PASSWORD`], the secret is a pure bearer capability:
/// whoever holds this string can read the share. The web client puts it in the
/// path (`/files/<ticket>`, `/info/<ticket>`) so those views are shareable as
/// ordinary URLs. With the flag set, the string alone is inert — it is the
/// *addressing*, and the password is the credential — which is what makes such
/// a URL safe to post where the password is not.
#[derive(Debug, Clone)]
pub struct MountTicket {
    pub addr: EndpointAddr,
    pub secret: [u8; SECRET_LEN],
    pub lookups: LookupOpts,
    pub kind: u8,
    /// Bit set of [`TICKET_FLAG_PASSWORD`] and whatever follows it. Zero for an
    /// ordinary share, and zero is not written to the wire.
    pub flags: u8,
    /// The share mesh's fofoca id, carried **only** on a password-protected
    /// share and `None` otherwise.
    ///
    /// An ordinary share needs no such field: every peer derives the same mesh
    /// from the ticket secret with no coordination, which is what
    /// [`crate::mesh_key::share_mesh_key`] is for. A protected share cannot,
    /// because the id a producer mints carries the **password verifier** that
    /// `fofoca`'s `Mesh::set_password` baked into it — sixteen bytes a joiner
    /// has no way to derive and must be given.
    ///
    /// Handing that id to `fofoca`'s `JoinParams` is what moves the
    /// wrong-password check off the network: `resolve` decodes the id, stretches
    /// the password, compares it against the verifier, and fails locally. A
    /// producer that is switched off does not weaken the check, which matters
    /// because a share is designed to outlive its producer.
    pub mesh_id: Option<String>,
    /// The creator's **authorship** public key, carried when
    /// [`TICKET_FLAG_SIGNED`] is set.
    ///
    /// Deliberately not the endpoint key already in `addr`. `agent-share mirror`
    /// hands the endpoint secret to every copy on purpose, so signing with it
    /// would make impersonation convincing rather than impossible. This key
    /// never leaves the creator's machine; see [`crate::authorship`].
    pub author: Option<[u8; 32]>,
}

/// Read the length-prefixed mesh id a protected ticket carries at `pos`.
///
/// `Ok(None)` for a ticket that stops before the field, or declares it empty —
/// both mean "this producer did not mint one", which is a supported shape and
/// not a malformed ticket.
///
/// # Errors
/// The length is present but the bytes behind it are truncated or not UTF-8.
fn read_mesh_id(payload: &[u8], pos: usize) -> Result<Option<String>> {
    let Some(len_bytes) = payload.get(pos..pos + 2) else {
        return Ok(None);
    };
    let len = usize::from(u16::from_le_bytes([len_bytes[0], len_bytes[1]]));
    if len == 0 {
        return Ok(None);
    }
    let raw = payload
        .get(pos + 2..pos + 2 + len)
        .context("ticket mesh id truncated")?;
    let mesh_id = std::str::from_utf8(raw).context("ticket mesh id is not utf-8")?;
    Ok(Some(mesh_id.to_owned()))
}

impl MountTicket {
    /// Whether redeeming this ticket also needs a password — see
    /// [`TICKET_FLAG_PASSWORD`].
    #[must_use]
    pub const fn password_protected(&self) -> bool {
        self.flags & TICKET_FLAG_PASSWORD != 0
    }

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
        // Only when non-zero, so an ordinary share's ticket is byte-for-byte
        // what it was before this field existed. A reader that predates it
        // never sees a byte it has to know about.
        if self.flags != 0 {
            payload.push(self.flags);
            // Length-prefixed for the same reason the address is: so a field
            // appended after it stays possible. Written inside the `flags`
            // branch because it only exists on a protected share, and an
            // ordinary ticket must not grow a length of zero it never had.
            let mesh_id = self.mesh_id.as_deref().unwrap_or_default();
            payload.extend_from_slice(
                &u16::try_from(mesh_id.len())
                    .expect("a mesh id is orders of magnitude under 64 KiB")
                    .to_le_bytes(),
            );
            payload.extend_from_slice(mesh_id.as_bytes());
            // After the mesh id, so a reader that predates this field stops at
            // the same place it always did and simply ignores the tail.
            if let Some(author) = self.author {
                payload.extend_from_slice(&author);
            }
        }
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
        // Absent means zero: an ordinary share never writes this byte.
        let flags = payload.get(pos).copied().unwrap_or(0);
        pos += usize::from(flags != 0);
        // Absent on an ordinary share, and tolerated as absent even on a
        // protected one: a ticket minted before this field existed still
        // decodes, and simply falls back to the producer-side refusal.
        let mesh_id = if flags == 0 {
            None
        } else {
            read_mesh_id(&payload, pos)?
        };
        // The authorship key sits past the mesh id, which is length-prefixed —
        // so its position depends on how long that id was, even when empty.
        let author = if flags & TICKET_FLAG_SIGNED == 0 {
            None
        } else {
            let after_mesh = pos
                + 2
                + payload
                    .get(pos..pos + 2)
                    .map_or(0, |raw| usize::from(u16::from_le_bytes([raw[0], raw[1]])));
            payload.get(after_mesh..after_mesh + 32).map(|raw| {
                let mut key = [0u8; 32];
                key.copy_from_slice(raw);
                key
            })
        };
        // Trailing bytes past the mesh id are tolerated, not rejected: that is
        // what makes the layout extensible. A later field appended here is
        // ignored by this build rather than failing it.
        let value: serde_json::Value =
            serde_json::from_slice(addr_json).context("invalid ticket address json")?;
        let (_id, addr) = endpoint_addr_from_json(&value)?;
        Ok(Self {
            addr,
            secret,
            lookups,
            kind,
            flags,
            mesh_id,
            author,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MountTicket, SECRET_LEN, TICKET_FLAG_PASSWORD, TICKET_KIND_BENCH_QUIC,
        TICKET_KIND_BENCH_RELAY, TICKET_KIND_BENCH_WEBRTC, TICKET_KIND_SHARE,
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
            flags: 0,
            mesh_id: None,
            author: None,
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
        assert_eq!(decoded.flags, 0);
        assert!(!decoded.password_protected());
    }

    #[test]
    fn the_password_flag_round_trips() {
        let mut ticket = sample();
        ticket.flags = TICKET_FLAG_PASSWORD;
        let decoded = MountTicket::decode(&ticket.encode()).expect("decode");
        assert_eq!(decoded.flags, TICKET_FLAG_PASSWORD);
        assert!(decoded.password_protected());
        // The secret still travels in the clear — it is the addressing, not the
        // credential. What the flag says is that it is no longer sufficient.
        assert_eq!(decoded.secret, ticket.secret);
    }

    #[test]
    fn the_mesh_id_round_trips_on_a_protected_ticket() {
        let mut ticket = sample();
        ticket.flags = TICKET_FLAG_PASSWORD;
        ticket.mesh_id = Some("some-fofoca-mesh-id".to_owned());
        let decoded = MountTicket::decode(&ticket.encode()).expect("decode");
        assert_eq!(decoded.mesh_id.as_deref(), Some("some-fofoca-mesh-id"));
    }

    /// A protected ticket minted before the mesh id existed must still decode.
    /// It simply carries none, and the consumer falls back to the producer-side
    /// refusal it always had.
    #[test]
    fn a_protected_ticket_without_a_mesh_id_still_decodes() {
        let ticket = sample();
        let (kind, mut payload) = token::decode(&ticket.encode()).expect("decode token");
        payload.push(TICKET_FLAG_PASSWORD);
        let older = token::encode(kind, &payload);

        let decoded = MountTicket::decode(&older).expect("a mesh-id-less ticket must decode");
        assert!(decoded.password_protected());
        assert_eq!(decoded.mesh_id, None);
    }

    /// The mesh id is only meaningful beside the flag, so an unprotected ticket
    /// never writes one even when the field is populated by mistake.
    #[test]
    fn an_unprotected_ticket_never_writes_a_mesh_id() {
        let mut ticket = sample();
        ticket.mesh_id = Some("ignored".to_owned());
        let decoded = MountTicket::decode(&ticket.encode()).expect("decode");
        assert_eq!(decoded.mesh_id, None);
        assert_eq!(decoded.flags, 0);
    }

    /// The reason `flags` is written only when non-zero: an ordinary share's
    /// ticket must be the same string it was before the field existed, so no
    /// unprotected share needs a compatibility story at all.
    #[test]
    fn a_passwordless_ticket_is_byte_identical_to_the_pre_flags_format() {
        let ticket = sample();
        let (_kind, payload) = token::decode(&ticket.encode()).expect("decode token");

        // The pre-flags payload, built the way the old encoder built it.
        let mut expected = Vec::new();
        expected.extend_from_slice(&ticket.secret);
        expected.push(ticket.kind);
        ticket.lookups.encode_into(&mut expected);
        let addr_json = serde_json::to_vec(&endpoint_addr_to_json(&ticket.addr)).expect("json");
        expected.extend_from_slice(
            &u16::try_from(addr_json.len())
                .expect("addr json fits u16")
                .to_le_bytes(),
        );
        expected.extend_from_slice(&addr_json);

        assert_eq!(payload, expected, "an unprotected ticket grew a byte");
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
    ///
    /// `flags` is the field that landed in that room, so the unknown bytes now
    /// go after it. That is the shape every later field inherits: append, and
    /// older builds stop reading where their knowledge does.
    #[test]
    fn a_field_appended_after_the_flags_is_ignored() {
        let ticket = sample();
        let (kind, mut payload) = token::decode(&ticket.encode()).expect("decode token");
        payload.push(0);
        payload.extend_from_slice(b"a future field this build knows nothing about");
        let extended = token::encode(kind, &payload);

        let decoded = MountTicket::decode(&extended).expect("trailing bytes must not be fatal");
        assert_eq!(decoded.addr.id, ticket.addr.id);
        assert_eq!(decoded.secret, ticket.secret);
        assert_eq!(decoded.flags, 0);
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

#[cfg(test)]
mod author_tests {
    use super::{
        MountTicket, SECRET_LEN, TICKET_FLAG_PASSWORD, TICKET_FLAG_SIGNED, TICKET_KIND_SHARE,
    };
    use crate::lookup::LookupOpts;
    use fofoca_protocol::iroh_base::{EndpointAddr, SecretKey};

    fn base() -> MountTicket {
        let id = SecretKey::from_bytes(&[7u8; 32]).public();
        MountTicket {
            addr: EndpointAddr::new(id).with_ip_addr("127.0.0.1:4242".parse().expect("addr")),
            secret: [5u8; SECRET_LEN],
            lookups: LookupOpts::public_preset(),
            kind: TICKET_KIND_SHARE,
            flags: 0,
            mesh_id: None,
            author: None,
        }
    }

    #[test]
    fn a_signed_ticket_carries_its_authorship_key() {
        let mut ticket = base();
        ticket.flags = TICKET_FLAG_SIGNED;
        ticket.author = Some([3u8; 32]);
        let decoded = MountTicket::decode(&ticket.encode()).expect("decode");
        assert_eq!(decoded.flags, TICKET_FLAG_SIGNED);
        assert_eq!(decoded.author, Some([3u8; 32]));
    }

    /// The key sits *past* the length-prefixed mesh id, so its offset depends on
    /// how long that id is. Both flags together is the case that would catch an
    /// offset computed as if the mesh id were never there.
    #[test]
    fn a_signed_and_protected_ticket_carries_both_fields() {
        let mut ticket = base();
        ticket.flags = TICKET_FLAG_PASSWORD | TICKET_FLAG_SIGNED;
        ticket.mesh_id = Some("a-fairly-long-mesh-identifier-here".to_owned());
        ticket.author = Some([9u8; 32]);
        let decoded = MountTicket::decode(&ticket.encode()).expect("decode");
        assert_eq!(
            decoded.mesh_id.as_deref(),
            Some("a-fairly-long-mesh-identifier-here")
        );
        assert_eq!(decoded.author, Some([9u8; 32]));
    }

    /// **An unsigned ticket must be byte-for-byte what it was.** This field is
    /// only reachable through a flag, so a share that does not use it pays
    /// nothing and older readers are unaffected.
    #[test]
    fn an_unsigned_ticket_is_unchanged() {
        let ticket = base();
        let decoded = MountTicket::decode(&ticket.encode()).expect("decode");
        assert_eq!(decoded.author, None);
        assert_eq!(decoded.flags, 0);
        // A protected-but-unsigned ticket must not grow the field either.
        let mut protected = base();
        protected.flags = TICKET_FLAG_PASSWORD;
        protected.mesh_id = Some("mesh".to_owned());
        let protected = MountTicket::decode(&protected.encode()).expect("decode");
        assert_eq!(protected.author, None);
    }

    /// A ticket claiming to be signed but truncated before the key decodes with
    /// `None` rather than failing — the producer-side refusal then applies, the
    /// same way a missing mesh id is handled.
    #[test]
    fn a_signed_ticket_missing_its_key_decodes_as_absent() {
        let mut ticket = base();
        ticket.flags = TICKET_FLAG_SIGNED;
        ticket.author = None;
        let decoded = MountTicket::decode(&ticket.encode()).expect("decode");
        assert_eq!(decoded.author, None);
    }
}
