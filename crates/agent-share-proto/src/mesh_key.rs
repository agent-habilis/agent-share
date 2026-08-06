//! The mesh a share belongs to, derived from the share's own token.
//!
//! Everyone holding the link — the producer, a CLI consumer, a browser tab —
//! computes the same value locally, so co-viewers of a share become peers on
//! one mesh with **no ticket format change and no new flag**. Before this they
//! were strangers: the ticket carries only the producer's address, so two tabs
//! on the same share had no way to learn of each other.
//!
//! Two hashes, not one, and the second is the point:
//!
//! ```text
//! share_mesh_key(token) = hex( SHA256("agent-share/mesh/v1" ‖ token) )
//!                         └── fed to the mesh's own topic derivation
//! ```
//!
//! The input is [`crate::auth::share_token`], not the ticket secret. On an
//! unprotected share those are the same 32 bytes, so this derivation is
//! unchanged. On a passworded one they are not, and that is what puts the mesh
//! behind the password too: someone holding the link without the password
//! cannot compute this id, so they do not merely fail to *read* the share —
//! they never find its peers, its tree fingerprint or its serving grid either.
//!
//! The mesh engine derives a topic mesh from a *string*, and carries that
//! string around — into `SetupKind::Topic`, the session state file, and
//! user-facing "joined topic …" lines. Handing it the bearer secret directly
//! would print the secret in all of those. Hashing first means the string is
//! already one-way, so a leaked mesh id or a state file reveals nothing about
//! the share it belongs to.
//!
//! Same trust boundary as the link itself in the other direction: anyone who
//! can compute this already holds the token, and the token already grants full
//! read access.

use sha2::{Digest, Sha256};

use crate::framing::SECRET_LEN;

/// Domain separator. Distinct from every other label in the tree so this
/// derivation can never collide with one of the mesh engine's own.
const SHARE_MESH_LABEL: &[u8] = b"agent-share/mesh/v1";

/// The topic string identifying a share's mesh, as lowercase hex.
///
/// Feed it to the mesh engine's topic derivation (`runtime::derive_topic_mesh`)
/// to get the `Mesh` every holder of this share's token agrees on. Pass
/// [`crate::auth::share_token`], not the raw ticket secret — see the module
/// docs for why the difference is the whole point on a passworded share.
#[must_use]
pub fn share_mesh_key(token: &[u8; SECRET_LEN]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(SHARE_MESH_LABEL);
    hasher.update(token);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        // `expect`-free: writing to a String cannot fail.
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{SECRET_LEN, share_mesh_key};

    #[test]
    fn is_deterministic_and_hex() {
        let secret = [7u8; SECRET_LEN];
        let first = share_mesh_key(&secret);
        assert_eq!(first, share_mesh_key(&secret), "derivation is not stable");
        assert_eq!(first.len(), 64, "expected a hex SHA-256");
        assert!(first.chars().all(|character| character.is_ascii_hexdigit()));
    }

    #[test]
    fn distinct_secrets_give_distinct_meshes() {
        let one = share_mesh_key(&[1u8; SECRET_LEN]);
        let two = share_mesh_key(&[2u8; SECRET_LEN]);
        assert_ne!(one, two, "two shares must not land on one mesh");
    }

    /// The whole reason for hashing before handing the string to the engine:
    /// the topic string is carried into the state file and user-facing lines,
    /// so it must not be the bearer secret.
    #[test]
    fn does_not_leak_the_secret() {
        let secret = [0xABu8; SECRET_LEN];
        let key = share_mesh_key(&secret);
        let secret_hex = secret.iter().fold(String::new(), |mut hex, byte| {
            use std::fmt::Write as _;
            write!(hex, "{byte:02x}").expect("writing to a String cannot fail");
            hex
        });
        assert!(!key.contains(&secret_hex), "derivation echoed the secret");
    }
}
