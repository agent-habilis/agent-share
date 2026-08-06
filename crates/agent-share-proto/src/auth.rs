//! The share token — the 32 bytes a mount request presents and a producer
//! expects, with or without a password.
//!
//! A ticket carries a random 32-byte `secret`. Until this module existed that
//! secret *was* the capability: it went on the wire verbatim and it derived the
//! share's mesh id. A password turns it into one of two factors instead:
//!
//! ```text
//! token = secret                                       (no password)
//! token = Argon2id(password, salt = f(secret, label))  (password)
//! ```
//!
//! Same 32 bytes either way, so nothing downstream changes shape — the request
//! header, [`crate::mesh_key::share_mesh_key`] and the hash-cache path all take
//! "the share token" where they used to take "the share secret". The
//! passwordless branch returns the secret *verbatim*, which is what keeps every
//! byte this build emits identical to the byte it emitted before.
//!
//! The stretch itself is [`fofoca_protocol::TicketAuth`] — the same primitive
//! fofoca uses for passworded blob tickets, at the same frozen Argon2id
//! parameters (19 `MiB`, t=2, p=1). Two consequences worth knowing before
//! calling this:
//!
//! - It costs ~100 ms and 19 `MiB`, synchronously, on every target including
//!   wasm. Derive once per share and hold the result; never per request.
//! - Those parameters are a network-wide contract in fofoca. This crate does not
//!   get to tune them.
//!
//! **There is deliberately no verifier.** A wrong password is not detectable
//! offline — it is detected by the producer refusing the connection
//! ([`crate::framing::CLOSE_UNAUTHORIZED`]), which keeps guessing online-only
//! and rate-limited by the network rather than by the guesser's CPU. The cost is
//! that a wrong password against a *dead* origin is indistinguishable from a
//! share that is simply gone, so callers must word that error to cover both.

use fofoca_protocol::{Password, TicketAuth};

use crate::framing::{CLOSE_BAD_SECRET, CLOSE_UNAUTHORIZED, SECRET_LEN};

/// Byte-domain for the mount ticket's password stretch.
///
/// The label is mixed into the derivation, so it reaches the wire and is fixed
/// for the life of this ticket kind — changing it strands every passworded
/// ticket ever minted. It is distinct from fofoca's own `b"blob-ticket"` on
/// purpose: two ticket kinds sharing a secret must never derive the same token.
pub const MOUNT_TICKET_LABEL: &[u8] = b"agent-share/mount-ticket/v1";

/// The 32 bytes every mount request presents and every producer compares.
///
/// With `None` this returns `*secret` — today's wire, unchanged, and the reason
/// an unprotected share needs no compatibility story.
///
/// Expensive with `Some`: see the module docs. Call it once per share.
#[must_use]
pub fn share_token(secret: &[u8; SECRET_LEN], password: Option<&str>) -> [u8; SECRET_LEN] {
    let password = password.map(|value| Password::new(value.to_owned()));
    TicketAuth::derive(secret, password.as_ref(), MOUNT_TICKET_LABEL).token
}

/// Constant-time equality for two share tokens.
///
/// Re-exported rather than reimplemented: a short-circuiting `==` on the token
/// is a timing oracle on the credential, and there is no reason for this crate
/// to own a second copy of the fold.
pub use fofoca_protocol::ct_eq;

/// What a producer checks an inbound mount request against.
///
/// The token and the "is this share protected" bit travel together because the
/// producer needs both at the same moment: one to decide *whether* to serve,
/// the other to decide **how to say no** — [`CLOSE_UNAUTHORIZED`] invites
/// another try with a different password, [`CLOSE_BAD_SECRET`] does not.
///
/// The flag describes this producer's own share, never a guess at why the peer
/// failed, so the close code reveals nothing the ticket's flag did not already.
///
/// Carried by the native producer and the browser one alike, so there is one
/// implementation of "is this request allowed" rather than two that drift.
#[derive(Clone, Copy)]
pub struct ShareAuth {
    token: [u8; SECRET_LEN],
    password_protected: bool,
}

impl ShareAuth {
    /// Derive from the ticket secret this producer minted (or inherited) and
    /// the password it was asked to protect the share with.
    ///
    /// Expensive with `Some` — see the module docs. Build one per share.
    #[must_use]
    pub fn new(secret: &[u8; SECRET_LEN], password: Option<&str>) -> Self {
        Self {
            token: share_token(secret, password),
            password_protected: password.is_some(),
        }
    }

    /// Adopt an already-derived token — a mirror re-serving from its sidecar,
    /// where the password was supplied once, at copy time, and is long gone.
    #[must_use]
    pub const fn from_token(token: [u8; SECRET_LEN], password_protected: bool) -> Self {
        Self {
            token,
            password_protected,
        }
    }

    /// The bytes a request must present, and the input to
    /// [`crate::mesh_key::share_mesh_key`].
    #[must_use]
    pub const fn token(&self) -> &[u8; SECRET_LEN] {
        &self.token
    }

    /// Whether this share is password-protected — the bit that becomes
    /// [`crate::ticket::TICKET_FLAG_PASSWORD`].
    #[must_use]
    pub const fn password_protected(&self) -> bool {
        self.password_protected
    }

    /// Whether a request header's leading bytes open this share. Constant-time,
    /// and `false` for anything shorter than a token rather than a panic.
    #[must_use]
    pub fn accepts(&self, presented: &[u8]) -> bool {
        let Some(bytes) = presented.get(..SECRET_LEN) else {
            return false;
        };
        let Ok(bytes) = <[u8; SECRET_LEN]>::try_from(bytes) else {
            return false;
        };
        ct_eq(&bytes, &self.token)
    }

    /// The connection close code for a request this share refuses.
    #[must_use]
    pub const fn refusal_code(&self) -> u32 {
        if self.password_protected {
            CLOSE_UNAUTHORIZED
        } else {
            CLOSE_BAD_SECRET
        }
    }

    /// The close reason paired with [`Self::refusal_code`].
    #[must_use]
    pub const fn refusal_reason(&self) -> &'static [u8] {
        if self.password_protected {
            b"unauthorized"
        } else {
            b"bad secret"
        }
    }
}

impl std::fmt::Debug for ShareAuth {
    /// The token is the live credential — redact it, keep the flag. Same shape
    /// as `fofoca`'s own `TicketAuth`, and for the same reason: these values
    /// end up inside handler structs that derive `Debug` and get logged.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ShareAuth")
            .field("token", &"***")
            .field("password_protected", &self.password_protected)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CLOSE_BAD_SECRET, CLOSE_UNAUTHORIZED, MOUNT_TICKET_LABEL, SECRET_LEN, ShareAuth, ct_eq,
        share_token,
    };

    /// The compatibility guarantee the whole design rests on: an unprotected
    /// share puts the ticket secret on the wire verbatim, exactly as it did
    /// before tokens existed.
    #[test]
    fn no_password_is_the_secret_verbatim() {
        let secret = [7u8; SECRET_LEN];
        assert_eq!(share_token(&secret, None), secret);
    }

    #[test]
    fn a_password_moves_the_token_off_the_secret() {
        let secret = [7u8; SECRET_LEN];
        let token = share_token(&secret, Some("hunter2"));
        assert_ne!(token, secret, "the secret must not survive the stretch");
    }

    #[test]
    fn derivation_is_deterministic() {
        let secret = [3u8; SECRET_LEN];
        assert_eq!(
            share_token(&secret, Some("hunter2")),
            share_token(&secret, Some("hunter2")),
        );
    }

    #[test]
    fn distinct_passwords_and_secrets_give_distinct_tokens() {
        let secret = [3u8; SECRET_LEN];
        assert_ne!(
            share_token(&secret, Some("hunter2")),
            share_token(&secret, Some("hunter3")),
        );
        assert_ne!(
            share_token(&secret, Some("hunter2")),
            share_token(&[4u8; SECRET_LEN], Some("hunter2")),
        );
    }

    /// The salt is keyed by the secret, so the same password on two shares is
    /// two unrelated tokens — one cracked share tells you nothing about another.
    #[test]
    fn the_salt_is_per_share() {
        let one = share_token(&[1u8; SECRET_LEN], Some("same"));
        let two = share_token(&[2u8; SECRET_LEN], Some("same"));
        assert_ne!(one, two);
    }

    #[test]
    fn ct_eq_agrees_with_equality() {
        let secret = [9u8; SECRET_LEN];
        let token = share_token(&secret, Some("hunter2"));
        assert!(ct_eq(&token, &share_token(&secret, Some("hunter2"))));
        assert!(!ct_eq(&token, &secret));
    }

    /// The label reaches the wire. Pinned so a rename cannot silently strand
    /// every passworded ticket already in circulation.
    #[test]
    fn the_label_is_pinned_wire_format() {
        assert_eq!(MOUNT_TICKET_LABEL, b"agent-share/mount-ticket/v1");
    }

    #[test]
    fn an_unprotected_share_accepts_its_ticket_secret() {
        let secret = [11u8; SECRET_LEN];
        let auth = ShareAuth::new(&secret, None);
        assert!(!auth.password_protected());
        assert!(auth.accepts(&secret));
        assert_eq!(auth.refusal_code(), CLOSE_BAD_SECRET);
    }

    /// The property the whole feature buys: holding the ticket is not enough.
    #[test]
    fn a_protected_share_refuses_its_own_ticket_secret() {
        let secret = [11u8; SECRET_LEN];
        let auth = ShareAuth::new(&secret, Some("hunter2"));
        assert!(auth.password_protected());
        assert!(!auth.accepts(&secret), "the ticket alone must not open it");
        assert!(auth.accepts(&share_token(&secret, Some("hunter2"))));
        assert!(!auth.accepts(&share_token(&secret, Some("hunter3"))));
        assert_eq!(auth.refusal_code(), CLOSE_UNAUTHORIZED);
    }

    /// A request header is `token(32) ‖ op(1)`, so the check reads a prefix and
    /// must not care what follows — nor panic on a header that never arrived.
    #[test]
    fn accepts_reads_a_prefix_and_survives_a_short_one() {
        let secret = [11u8; SECRET_LEN];
        let auth = ShareAuth::new(&secret, None);
        let mut header = secret.to_vec();
        header.push(1);
        assert!(auth.accepts(&header));
        assert!(!auth.accepts(&[]));
        assert!(!auth.accepts(&secret[..31]));
    }

    #[test]
    fn a_mirror_can_adopt_a_derived_token() {
        let secret = [11u8; SECRET_LEN];
        let token = share_token(&secret, Some("hunter2"));
        let auth = ShareAuth::from_token(token, true);
        assert!(auth.accepts(&token));
        assert!(!auth.accepts(&secret));
        assert_eq!(auth.token(), &token);
    }

    #[test]
    fn debug_redacts_the_token() {
        let auth = ShareAuth::new(&[11u8; SECRET_LEN], Some("hunter2"));
        let rendered = format!("{auth:?}");
        assert!(rendered.contains("***"));
        assert!(
            !rendered.contains("11"),
            "the token must not print: {rendered}"
        );
    }
}
