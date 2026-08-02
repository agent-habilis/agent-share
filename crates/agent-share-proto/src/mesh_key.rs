//! The mesh a share belongs to, derived from the share's own secret.
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
//! share_mesh_key(secret) = hex( SHA256("agent-share/mesh/v1" ‖ secret) )
//!                          └── fed to the mesh's own topic derivation
//! ```
//!
//! The mesh engine derives a topic mesh from a *string*, and carries that
//! string around — into `SetupKind::Topic`, the session state file, and
//! user-facing "joined topic …" lines. Handing it the bearer secret directly
//! would print the secret in all of those. Hashing first means the string is
//! already one-way, so a leaked mesh id or a state file reveals nothing about
//! the share it belongs to.
//!
//! Same trust boundary as the link itself in the other direction: anyone who
//! can compute this already holds the secret, and the secret already grants
//! full read access.

use sha2::{Digest, Sha256};

use crate::framing::SECRET_LEN;

/// Domain separator. Distinct from every other label in the tree so this
/// derivation can never collide with one of the mesh engine's own.
const SHARE_MESH_LABEL: &[u8] = b"agent-share/mesh/v1";

/// The topic string identifying a share's mesh, as lowercase hex.
///
/// Feed it to the mesh engine's topic derivation (`runtime::derive_topic_mesh`)
/// to get the `Mesh` every holder of this ticket agrees on.
#[must_use]
pub fn share_mesh_key(secret: &[u8; SECRET_LEN]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(SHARE_MESH_LABEL);
    hasher.update(secret);
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
