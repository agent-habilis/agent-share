//! [`SwarmId`] — the validated swarm id *string* (shallow: length +
//! Base58 charset). Cheap boundary check at the CLI / IPC edge; full
//! structural decoding lives in [`super::Swarm`].

use std::borrow::Borrow;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

const MIN_LEN: usize = 3;
const MAX_LEN: usize = 512;

/// A swarm identifier — the encoded Base58Check string.
///
/// Validation is shallow: length 3..=512 and the Base58 charset
/// (`[1-9A-HJ-NP-Za-km-z]`). Full structural decoding lives in
/// `Swarm::from_str`; the newtype rejects obvious typos at the
/// CLI / IPC boundary without paying the decode cost on every flow.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct SwarmId(String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SwarmIdError {
    Length(usize),
    Charset(String),
}

impl fmt::Display for SwarmIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SwarmIdError::Length(len) => {
                write!(
                    formatter,
                    "swarm id must be {MIN_LEN}..={MAX_LEN} chars, got {len}"
                )
            }
            SwarmIdError::Charset(value) => {
                write!(formatter, "swarm id has invalid Base58 char(s): {value:?}")
            }
        }
    }
}

impl std::error::Error for SwarmIdError {}

fn is_base58_char(ch: char) -> bool {
    matches!(ch,
        '1'..='9'
        | 'A'..='H' | 'J'..='N' | 'P'..='Z'
        | 'a'..='k' | 'm'..='z'
    )
}

impl SwarmId {
    pub(crate) fn new(value: impl Into<String>) -> Result<Self, SwarmIdError> {
        let value = value.into();
        if value.len() < MIN_LEN || value.len() > MAX_LEN {
            return Err(SwarmIdError::Length(value.len()));
        }
        if !value.chars().all(is_base58_char) {
            return Err(SwarmIdError::Charset(value));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SwarmId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for SwarmId {
    type Err = SwarmIdError;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::new(text)
    }
}

impl AsRef<str> for SwarmId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for SwarmId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
impl From<&str> for SwarmId {
    fn from(text: &str) -> Self {
        Self::new(text).expect("invalid swarm id in test fixture")
    }
}

#[cfg(test)]
mod swarm_id_tests {
    use super::{SwarmId, SwarmIdError};

    #[test]
    fn new_accepts_well_formed_id() {
        SwarmId::new("AbCdEf1234").unwrap();
    }

    #[test]
    fn new_rejects_too_short() {
        assert!(matches!(SwarmId::new("a"), Err(SwarmIdError::Length(_))));
    }

    #[test]
    fn new_rejects_invalid_base58_chars() {
        // `0`, `O`, `I`, `l` are not in the Base58 alphabet — and neither is
        // the emoji prefix ids used to carry, so a stale paste lands here.
        assert!(matches!(
            SwarmId::new("AbCdEf0xyz"),
            Err(SwarmIdError::Charset(_))
        ));
        assert!(matches!(
            SwarmId::new("🐝AbCdEf1234"),
            Err(SwarmIdError::Charset(_))
        ));
    }

    #[test]
    fn serde_transparent_round_trip() {
        let id = SwarmId::from("AbCdEf1234");
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"AbCdEf1234\"");
        let parsed: SwarmId = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, id);
    }
}
