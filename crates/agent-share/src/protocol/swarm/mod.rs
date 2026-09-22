//! The swarm identifier, vendored from agent-habilis/swarm's
//! `src/protocol/swarm/mod.rs` — trimmed to what mount needs from a
//! `--swarm` id: its embedded lookup allowlist. The full `Swarm` type
//! (seed, name semantics, password crypto, topic derivation) stays in
//! swarm; [`swarm_id_lookups`] walks the exact same wire framing
//! (`Swarm::decode_bytes` + `SwarmConfig::from_bytes`) but keeps only the
//! [`LookupOpts`], so old and new binaries accept the same id strings.

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

pub(crate) use self::id::SwarmId;
pub(crate) use self::lookup::{
    LookupOpts, LookupSet, RelayChoice, RelayLadder, RelaySelection, resolve_transfer_lookups,
};

mod id;
mod lookup;

/// Version byte leading the id payload; bumped on incompatible layout changes.
const VERSION: u8 = 1;
const SEED_LEN: usize = 32;
/// `SwarmName` is 1..=32 UTF-8 scalar values, so at most 32 * 4 bytes.
const NAME_MAX_BYTES: usize = 32 * 4;

/// Feature byte appended after the lookups when a swarm has a password.
const FEATURE_PASSWORD: u8 = 0b0001;
/// Length of the password verifier that follows the feature byte.
const PASSWORD_VERIFIER_LEN: usize = 16;

/// Extract the lookup allowlist embedded in a swarm id, skipping the
/// seed/name/password semantics mount has no use for. Mirrors swarm's
/// `Swarm::decode_bytes` framing byte-for-byte: `version u8 ‖ seed[32] ‖
/// name-len u8 ‖ name ‖ config-len u16 LE ‖ config`, all base58check-encoded.
/// One deliberate laxity: the name's *charset* is not re-validated (only
/// length + UTF-8) — a checksummed id with an invalid charset cannot be
/// minted by a real `ahsw`.
pub(crate) fn swarm_id_lookups(id: &str) -> Result<LookupOpts> {
    let bytes = base58check_decode(id)?;
    let mut pos = 0usize;
    let version = *bytes.get(pos).context("Swarm identifier too short")?;
    pos += 1;
    if version != VERSION {
        bail!("Unsupported swarm id version: {version}");
    }

    bytes
        .get(pos..pos + SEED_LEN)
        .context("Swarm identifier too short")?;
    pos += SEED_LEN;

    let name_len = *bytes.get(pos).context("Truncated swarm name length")? as usize;
    pos += 1;
    if name_len == 0 || name_len > NAME_MAX_BYTES {
        bail!("Invalid swarm name length: {name_len}");
    }
    let name_raw = bytes
        .get(pos..pos + name_len)
        .context("Truncated swarm name")?;
    std::str::from_utf8(name_raw).context("Invalid swarm name UTF-8")?;
    pos += name_len;

    let config_len =
        lookup::read_u16(&bytes, &mut pos).context("Truncated config length")? as usize;
    let config_raw = bytes
        .get(pos..pos + config_len)
        .context("Truncated swarm config")?;
    pos += config_len;
    if pos != bytes.len() {
        bail!("Trailing bytes in swarm identifier");
    }
    config_lookups(config_raw)
}

/// Decode a config region, requiring it to consume `bytes` exactly. Mirrors
/// swarm's `SwarmConfig::from_bytes` accept/reject behavior (unknown feature
/// flags rejected, the non-canonical zero feature byte rejected, a password
/// verifier length-checked and skipped) but returns only the lookups.
fn config_lookups(bytes: &[u8]) -> Result<LookupOpts> {
    let mut pos = 0;
    let lookups = LookupOpts::decode_from(bytes, &mut pos)?;
    if pos != bytes.len() {
        let features = bytes[pos];
        pos += 1;
        if features & !FEATURE_PASSWORD != 0 {
            bail!("unsupported swarm feature flags {features:#04x} — upgrade ahsw");
        }
        if features == 0 {
            // A zero feature byte re-encodes without itself, silently changing
            // the topic-derivation bytes — reject the non-canonical form.
            bail!("non-canonical swarm config: zero feature flags");
        }
        let end = pos
            .checked_add(PASSWORD_VERIFIER_LEN)
            .context("password verifier length overflow")?;
        bytes.get(pos..end).context("truncated password verifier")?;
        pos = end;
        if pos != bytes.len() {
            bail!("trailing bytes in swarm config");
        }
    }
    Ok(lookups)
}

fn checksum(bytes: &[u8]) -> [u8; 4] {
    let first = Sha256::digest(bytes);
    let second = Sha256::digest(first);
    let mut out = [0u8; 4];
    out.copy_from_slice(&second[..4]);
    out
}

fn base58check_decode(encoded: &str) -> Result<Vec<u8>> {
    let decoded = bs58::decode(encoded)
        .into_vec()
        .context("Invalid Base58 swarm encoding")?;
    if decoded.len() < 4 {
        bail!("Swarm identifier too short");
    }
    let (payload, received_checksum) = decoded.split_at(decoded.len() - 4);
    let expected_checksum = checksum(payload);
    if received_checksum != expected_checksum {
        bail!("Invalid swarm checksum");
    }
    Ok(payload.to_vec())
}

/// Mint a structurally valid swarm id for tests — the same framing a
/// real `ahsw create` produces (fixed dummy seed, no password).
#[cfg(test)]
pub(crate) fn encode_test_swarm_id(name: &str, lookups: &LookupOpts) -> String {
    let mut config = Vec::new();
    lookups.encode_into(&mut config);
    let mut buf = Vec::with_capacity(1 + SEED_LEN + 1 + name.len() + 2 + config.len());
    buf.push(VERSION);
    buf.extend_from_slice(&[7u8; SEED_LEN]);
    buf.push(u8::try_from(name.len()).expect("test name fits a u8 length"));
    buf.extend_from_slice(name.as_bytes());
    buf.extend_from_slice(
        &u16::try_from(config.len())
            .expect("test config fits a u16 length")
            .to_le_bytes(),
    );
    buf.extend_from_slice(&config);
    let mut with_checksum = buf.clone();
    with_checksum.extend_from_slice(&checksum(&buf));
    bs58::encode(with_checksum).into_string()
}

#[cfg(test)]
mod tests {
    use super::{LookupOpts, RelayChoice, config_lookups, encode_test_swarm_id, swarm_id_lookups};

    /// Encode `lookups` the way `SwarmConfig::to_bytes` does for a
    /// passwordless swarm (the lookups region alone, no feature byte).
    fn config_bytes(lookups: &LookupOpts) -> Vec<u8> {
        let mut buf = Vec::new();
        lookups.encode_into(&mut buf);
        buf
    }

    #[test]
    fn swarm_id_wire_format_is_pinned() {
        // Golden pin: the same id agent-habilis/swarm mints for
        // `Swarm::new([7u8; 32], SwarmName::new("test")?, SwarmConfig::loopback())`.
        // `tests/common::LOOPBACK_SWARM_ID` hardcodes it for the integration
        // test; if this ever changes, cross-repo interop broke.
        assert_eq!(
            encode_test_swarm_id("test", &LookupOpts::loopback()),
            "2UXAThUkdBAbiJNXvCt4YeMGQ9myFg7gJJZSr3pG3MAGzUwWmmV7D2Msw3sco"
        );
    }

    #[test]
    fn extracts_loopback_lookups_from_a_minted_id() {
        let id = encode_test_swarm_id("test", &LookupOpts::loopback());
        let lookups = swarm_id_lookups(&id).expect("decode");
        assert!(lookups.is_loopback());
    }

    #[test]
    fn extracts_public_preset_lookups_from_a_minted_id() {
        let id = encode_test_swarm_id("test", &LookupOpts::public_preset());
        let lookups = swarm_id_lookups(&id).expect("decode");
        assert_eq!(lookups, LookupOpts::public_preset());
    }

    #[test]
    fn extracts_a_custom_relay_ladder() {
        let opts = LookupOpts {
            mdns: true,
            dht: false,
            relay: RelayChoice::Custom(vec![
                "https://a.example".parse().unwrap(),
                "https://b.example".parse().unwrap(),
            ]),
        };
        let id = encode_test_swarm_id("test", &opts);
        assert_eq!(swarm_id_lookups(&id).expect("decode"), opts);
    }

    #[test]
    fn rejects_a_bad_checksum() {
        let id = encode_test_swarm_id("test", &LookupOpts::loopback());
        // Flip the last character to corrupt the checksum.
        let mut corrupted: String = id.chars().collect();
        let last = corrupted.pop().unwrap();
        corrupted.push(if last == '1' { '2' } else { '1' });
        assert!(swarm_id_lookups(&corrupted).is_err());
    }

    #[test]
    fn config_accepts_a_password_verifier_and_keeps_the_lookups() {
        let mut bytes = config_bytes(&LookupOpts::public_preset());
        bytes.push(super::FEATURE_PASSWORD);
        bytes.extend_from_slice(&[0xA5u8; super::PASSWORD_VERIFIER_LEN]);
        let lookups = config_lookups(&bytes).expect("passworded config decodes");
        assert_eq!(lookups, LookupOpts::public_preset());
    }

    #[test]
    fn config_rejects_unknown_feature_flags() {
        let mut bytes = config_bytes(&LookupOpts::public_preset());
        bytes.push(0b0010); // an undefined feature bit
        bytes.extend_from_slice(&[0u8; 16]);
        let error = config_lookups(&bytes).unwrap_err();
        assert!(error.to_string().contains("upgrade ahsw"), "got: {error}");
    }

    #[test]
    fn config_rejects_the_non_canonical_zero_feature_byte() {
        let mut bytes = config_bytes(&LookupOpts::loopback());
        bytes.push(0);
        assert!(config_lookups(&bytes).is_err());
    }

    #[test]
    fn config_rejects_a_truncated_verifier_and_trailing_slack() {
        let mut truncated = config_bytes(&LookupOpts::public_preset());
        truncated.push(super::FEATURE_PASSWORD);
        truncated.extend_from_slice(&[0u8; 8]); // half a verifier
        assert!(config_lookups(&truncated).is_err());

        let mut slack = config_bytes(&LookupOpts::public_preset());
        slack.push(super::FEATURE_PASSWORD);
        slack.extend_from_slice(&[0u8; 17]); // verifier + one extra byte
        assert!(config_lookups(&slack).is_err());
    }

    #[test]
    fn passwordless_encoding_matches_the_pinned_wire_bytes() {
        // The same golden bytes swarm pins: a config without a password must
        // stay byte-for-byte what it was before features existed.
        assert_eq!(config_bytes(&LookupOpts::loopback()), vec![0b0000]);
        assert_eq!(config_bytes(&LookupOpts::public_preset()), vec![0b0111]);
    }
}
