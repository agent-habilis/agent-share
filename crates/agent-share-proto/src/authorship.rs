//! Who is allowed to change a share.
//!
//! A share is meant to outlive its producer, which means the manifest a peer
//! finally believes usually did **not** come from the origin. Without a
//! signature the only fallback is a popularity contest — `bootstrap_from_seeders`
//! took the majority tree among peer cards, and its own comment admits ghost
//! cards from departed peers vote too. A handful of fabricated peers wins that.
//!
//! So the creator signs. Everyone holding the link can check, with no live
//! origin and no quorum.
//!
//! # Two keys, not one
//!
//! The obvious implementation signs with the endpoint key the ticket already
//! names. It is the wrong key, for a reason that is easy to miss: **the endpoint
//! key is per-peer and per-run.** `mount::produce::bind` mints a fresh one on
//! every `serve`, and says so — "the secret is the *share* capability, not this
//! peer's identity, and two peers must never share the latter". So it answers
//! *who is on the other end of this connection*, which is not the question a
//! manifest signature asks.
//!
//! Two things follow, and both matter here:
//!
//! - A seeder has its own endpoint key, so "signed by the endpoint you dialled"
//!   is a property no seeder can ever have. It could only re-serve the origin's
//!   signature, at which point the endpoint key was never doing the work.
//! - An origin that restarts comes up under a new endpoint key. Every manifest
//!   it published before would stop verifying against the link people already
//!   hold, and every one after would need a reissued ticket.
//!
//! So the two roles are split:
//!
//! - The **authorship key** names the share's creator across restarts and
//!   across peers. It signs manifests, its public half rides the ticket, and it
//!   is never written to a mirror's sidecar.
//! - The **serving identity** is the per-peer endpoint key. Untouched.
//!
//! A mirror is handed the share's read capability on purpose — that is what
//! makes a copy an extra source rather than a rival share — but never the
//! authorship key. So it can serve every byte and still not publish a version.
//! That is the requirement: *mutable share, mutable only by its creator*.
//!
//! # What a signature does not fix
//!
//! It stops forgery, not **rollback**: an old manifest the creator really did
//! sign stays valid forever. So the version rides inside the signed bytes and a
//! consumer keeps the highest it has seen. Withholding — a peer simply not
//! mentioning a newer version — remains possible and is inherent to any design
//! where the source may be offline.

use anyhow::{Result, bail};
use fofoca_protocol::iroh_base::Signature;

/// The keypair halves manifests are signed and checked with.
///
/// Re-exported so a caller names one type rather than reaching for whichever
/// of iroh's re-exports is closest — two paths to the same ed25519 key is the
/// kind of thing that compiles everywhere except where it matters.
pub use fofoca_protocol::iroh_base::{PublicKey, SecretKey};

/// Domain separator, so a manifest signature cannot be replayed as a signature
/// over anything else this keypair is ever asked to sign.
const CONTEXT: &[u8] = b"agent-share/manifest/v1";

/// Bytes of an ed25519 signature.
pub const SIGNATURE_LEN: usize = 64;

/// Bytes of an ed25519 public key.
pub const AUTHOR_KEY_LEN: usize = 32;

/// What the creator actually signs: the context, the version, and the manifest.
///
/// Built here rather than at each call site so the signer and the verifier
/// cannot drift apart — a mismatch would show up as "every manifest is forged",
/// which is a confusing way to learn about a missing separator.
fn signed_payload(version: u64, manifest: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(CONTEXT.len() + 8 + manifest.len());
    out.extend_from_slice(CONTEXT);
    out.extend_from_slice(&version.to_le_bytes());
    out.extend_from_slice(manifest);
    out
}

/// Sign a manifest version as the share's creator.
#[must_use]
pub fn sign_manifest(author: &SecretKey, version: u64, manifest: &[u8]) -> [u8; SIGNATURE_LEN] {
    author.sign(&signed_payload(version, manifest)).to_bytes()
}

/// Check a manifest against the creator's public key.
///
/// # Errors
/// The signature does not verify — meaning these bytes are not what the holder
/// of the authorship key published, whoever handed them over.
pub fn verify_manifest(
    author: &PublicKey,
    version: u64,
    manifest: &[u8],
    signature: &[u8; SIGNATURE_LEN],
) -> Result<()> {
    author
        .verify(
            &signed_payload(version, manifest),
            &Signature::from_bytes(signature),
        )
        .map_err(|error| {
            anyhow::anyhow!("the manifest is not signed by this share's creator: {error}")
        })
}

/// A manifest and the proof that its creator published it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedManifest {
    /// Monotonic, and inside the signature — see the rollback note above.
    pub version: u64,
    pub signature: [u8; SIGNATURE_LEN],
    /// The origin's manifest bytes, **verbatim**. The tree fingerprint is
    /// defined over exactly these, so they are carried rather than re-encoded.
    pub manifest: Vec<u8>,
}

impl SignedManifest {
    /// Wire layout: `version(u64) ‖ signature(64) ‖ manifest`.
    ///
    /// One shape whether or not the share is signed — an unsigned share sends a
    /// zero signature and its ticket carries no key, so nobody checks. A second
    /// shape would mean a peer could choose which one to answer with, and
    /// "unsigned" is not a thing a *reader* should get to decide.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + SIGNATURE_LEN + self.manifest.len());
        out.extend_from_slice(&self.version.to_le_bytes());
        out.extend_from_slice(&self.signature);
        out.extend_from_slice(&self.manifest);
        out
    }

    /// Decode what [`Self::encode`] wrote.
    ///
    /// # Errors
    /// The body is shorter than the fixed prefix.
    ///
    /// # Panics
    /// Never: both slices taken below sit inside the length checked
    /// immediately above them.
    pub fn decode(body: &[u8]) -> Result<Self> {
        const PREFIX: usize = 8 + SIGNATURE_LEN;
        if body.len() < PREFIX {
            bail!(
                "a signed manifest is at least {PREFIX} bytes, got {}",
                body.len()
            );
        }
        let version = u64::from_le_bytes(body[..8].try_into().expect("8 bytes"));
        let mut signature = [0u8; SIGNATURE_LEN];
        signature.copy_from_slice(&body[8..PREFIX]);
        Ok(Self {
            version,
            signature,
            manifest: body[PREFIX..].to_vec(),
        })
    }

    /// Whether this is what `author` published, at a version worth taking.
    ///
    /// `seen` is the highest version this peer has already accepted. Equal is
    /// allowed — the same manifest arriving twice is ordinary — but lower is
    /// refused, which is what makes replaying an old signed manifest useless.
    ///
    /// # Errors
    /// The signature does not verify, or the version went backwards.
    pub fn accept(&self, author: &PublicKey, seen: u64) -> Result<()> {
        if self.version < seen {
            bail!(
                "a peer offered version {} after version {seen}; refusing to roll back",
                self.version
            );
        }
        verify_manifest(author, self.version, &self.manifest, &self.signature)
    }
}

#[cfg(test)]
mod tests {
    use super::{SignedManifest, sign_manifest, verify_manifest};
    use fofoca_protocol::iroh_base::SecretKey;

    fn creator() -> SecretKey {
        SecretKey::from_bytes(&[7u8; 32])
    }

    fn signed(version: u64, manifest: &[u8]) -> SignedManifest {
        SignedManifest {
            version,
            signature: sign_manifest(&creator(), version, manifest),
            manifest: manifest.to_vec(),
        }
    }

    #[test]
    fn what_the_creator_signs_verifies() {
        let manifest = b"a tree".to_vec();
        let signature = sign_manifest(&creator(), 3, &manifest);
        assert!(verify_manifest(&creator().public(), 3, &manifest, &signature).is_ok());
    }

    /// The whole point: somebody else's key does not pass. A mirror holds the
    /// *serving* identity and still cannot publish.
    #[test]
    fn another_key_cannot_publish() {
        let manifest = b"a tree".to_vec();
        let impostor = SecretKey::from_bytes(&[9u8; 32]);
        let forged = sign_manifest(&impostor, 1, &manifest);
        assert!(verify_manifest(&creator().public(), 1, &manifest, &forged).is_err());
    }

    #[test]
    fn altered_bytes_do_not_verify() {
        let signature = sign_manifest(&creator(), 1, b"a tree");
        assert!(verify_manifest(&creator().public(), 1, b"a treE", &signature).is_err());
    }

    /// The version is *inside* the signature, so a peer cannot relabel a
    /// manifest as a newer one to win the highest-version rule.
    #[test]
    fn the_version_cannot_be_relabelled() {
        let manifest = b"a tree".to_vec();
        let signature = sign_manifest(&creator(), 1, &manifest);
        assert!(verify_manifest(&creator().public(), 2, &manifest, &signature).is_err());
    }

    #[test]
    fn a_signed_manifest_round_trips() {
        let original = signed(42, b"the bytes");
        assert_eq!(
            SignedManifest::decode(&original.encode()).expect("decode"),
            original
        );
    }

    #[test]
    fn a_truncated_body_is_refused() {
        assert!(SignedManifest::decode(&[0u8; 71]).is_err());
        // Exactly the prefix and no manifest is a real shape: an empty share.
        assert!(SignedManifest::decode(&[0u8; 72]).is_ok());
    }

    /// Rollback: an old manifest the creator really did sign stays valid
    /// forever, so the version rule is the only thing refusing it.
    #[test]
    fn an_older_signed_manifest_is_refused() {
        let old = signed(1, b"old tree");
        assert!(old.accept(&creator().public(), 5).is_err());
        assert!(old.accept(&creator().public(), 1).is_ok());
        // Equal is fine: the same manifest arriving twice is ordinary.
        assert!(signed(5, b"same").accept(&creator().public(), 5).is_ok());
    }

    /// A forged manifest at a *newer* version is still refused — the version
    /// check must not be reachable as a way around the signature.
    #[test]
    fn a_forged_newer_version_is_still_refused() {
        let impostor = SecretKey::from_bytes(&[9u8; 32]);
        let forged = SignedManifest {
            version: 99,
            signature: sign_manifest(&impostor, 99, b"mine now"),
            manifest: b"mine now".to_vec(),
        };
        assert!(forged.accept(&creator().public(), 1).is_err());
    }

    /// An unsigned share sends a zero signature. It must not accidentally
    /// verify against any key.
    #[test]
    fn a_zero_signature_never_verifies() {
        let unsigned = SignedManifest {
            version: 0,
            signature: [0u8; 64],
            manifest: b"a tree".to_vec(),
        };
        assert!(unsigned.accept(&creator().public(), 0).is_err());
    }
}
