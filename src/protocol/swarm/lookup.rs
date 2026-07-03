//! The lookup allowlist carried in the `🐝…` id (`mdns`/`dht`/`relay`) plus
//! its byte codec, vendored from agent-habilis/swarm's
//! `src/protocol/swarm/lookup.rs` — trimmed of the directory/advertise
//! machinery and `SwarmConfig` (whose lookups-only decode lives in
//! [`super::config_lookups`]). A swarm's network reach is fully described by
//! its lookups: no lookups means loopback-only; any lookup means reachable
//! across machines.

use std::fmt;
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use iroh::RelayUrl;

/// The connectivity relay. `Disabled` ⇒ no relay at all
/// (`RelayMode::Disabled`); `Pinned` ⇒ the lookup-layer pinned default
/// *ladder* (the n0 prod set); `Custom` ⇒ an operator-supplied **ordered
/// ladder** (`--relay a,b,c`). Relay is an allowlist member like
/// mdns/dht, not an always-on URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RelayChoice {
    Disabled,
    Pinned,
    Custom(Vec<RelayUrl>),
}

/// The lookup allowlist baked into the swarm id. `mdns`/`dht` are the
/// enabled iroh address-lookups; `relay` is the connectivity relay (see
/// [`RelayChoice`]). An all-off set is a loopback-only swarm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LookupOpts {
    pub mdns: bool,
    pub dht: bool,
    pub relay: RelayChoice,
}

/// Wire ceiling on a custom relay ladder, so a forged id can't blow up
/// allocation. Far above any real ladder.
const MAX_RELAY_LADDER: usize = 16;
/// Wire ceiling on a single relay URL's byte length.
const MAX_RELAY_URL_BYTES: usize = 512;

impl LookupOpts {
    /// Loopback-only: no address-lookups, no relay.
    pub(crate) fn loopback() -> Self {
        LookupOpts {
            mdns: false,
            dht: false,
            relay: RelayChoice::Disabled,
        }
    }

    /// The all-on default for a swarm reachable across machines: both
    /// address-lookups plus the pinned default relay ladder.
    pub(crate) fn public_preset() -> Self {
        LookupOpts {
            mdns: true,
            dht: true,
            relay: RelayChoice::Pinned,
        }
    }

    /// True when nothing reaches off-machine — the swarm is loopback-only.
    pub(crate) fn is_loopback(&self) -> bool {
        !self.mdns && !self.dht && self.relay == RelayChoice::Disabled
    }

    /// Human/JSON label for the swarm's reach. Derived from the lookups —
    /// there is no stored network mode.
    pub(crate) fn network_label(&self) -> &'static str {
        if self.is_loopback() {
            "private"
        } else {
            "public"
        }
    }

    /// Append the canonical wire encoding to `buf`:
    /// `[flags u8][if custom: [count u8] ([len u16 LE] url)*]`.
    pub(crate) fn encode_into(&self, buf: &mut Vec<u8>) {
        let mut flags: u8 = 0;
        if self.mdns {
            flags |= 0b0001;
        }
        if self.dht {
            flags |= 0b0010;
        }
        match &self.relay {
            RelayChoice::Disabled => {}
            RelayChoice::Pinned => flags |= 0b0100,
            RelayChoice::Custom(_) => flags |= 0b0100 | 0b1000,
        }
        buf.push(flags);
        if let RelayChoice::Custom(ladder) = &self.relay {
            // The ladder is created locally and bounded by the CLI/embed,
            // so this cast and the lengths below always fit.
            buf.push(u8::try_from(ladder.len()).expect("relay ladder bounded by MAX_RELAY_LADDER"));
            for url in ladder {
                let text = url.to_string();
                let len =
                    u16::try_from(text.len()).expect("relay URL bounded by MAX_RELAY_URL_BYTES");
                buf.extend_from_slice(&len.to_le_bytes());
                buf.extend_from_slice(text.as_bytes());
            }
        }
    }

    /// Decode from a cursor over the config region, advancing `pos`.
    pub(crate) fn decode_from(bytes: &[u8], pos: &mut usize) -> Result<Self> {
        let flags = *bytes.get(*pos).context("truncated lookup flags")?;
        *pos += 1;
        let mdns = flags & 0b0001 != 0;
        let dht = flags & 0b0010 != 0;
        let relay_enabled = flags & 0b0100 != 0;
        let relay_custom = flags & 0b1000 != 0;
        if relay_custom && !relay_enabled {
            bail!("custom-relay bit set without relay-enabled bit");
        }
        let relay = if !relay_enabled {
            RelayChoice::Disabled
        } else if !relay_custom {
            RelayChoice::Pinned
        } else {
            let count = *bytes.get(*pos).context("truncated relay ladder count")? as usize;
            *pos += 1;
            if count == 0 {
                bail!("custom relay ladder is empty");
            }
            if count > MAX_RELAY_LADDER {
                bail!("relay ladder too long: {count}");
            }
            let mut ladder = Vec::with_capacity(count);
            for _ in 0..count {
                let len = read_u16(bytes, pos).context("truncated relay URL length")? as usize;
                if len > MAX_RELAY_URL_BYTES {
                    bail!("relay URL too long: {len}");
                }
                let end = pos.checked_add(len).context("relay URL length overflow")?;
                let raw = bytes.get(*pos..end).context("truncated relay URL")?;
                *pos = end;
                let text = std::str::from_utf8(raw).context("relay URL is not UTF-8")?;
                ladder.push(text.parse::<RelayUrl>().context("invalid relay URL")?);
            }
            RelayChoice::Custom(ladder)
        };
        Ok(LookupOpts { mdns, dht, relay })
    }
}

pub(super) fn read_u16(bytes: &[u8], pos: &mut usize) -> Result<u16> {
    let end = pos.checked_add(2).context("u16 length overflow")?;
    let slice = bytes.get(*pos..end).context("truncated u16")?;
    *pos = end;
    Ok(u16::from_le_bytes([slice[0], slice[1]]))
}

/// Relay intent in a [`LookupSet`]: absent / default / custom. Resolved
/// into a `RelayChoice` by `resolve_lookups`. `Custom` carries the
/// ordered [`RelayLadder`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum RelaySelection {
    /// No relay (the CLI `--relay` flag absent).
    #[default]
    Unset,
    /// The pinned default n0 prod relay ladder (bare `--relay`).
    Default,
    /// A custom ordered ladder (`--relay a,b,c`).
    Custom(RelayLadder),
}

impl RelaySelection {
    fn is_set(&self) -> bool {
        !matches!(self, RelaySelection::Unset)
    }
}

/// The lookup flags the user selected on the CLI. `mdns`/`dht` are
/// address-lookups; `relay` is the connectivity/relay-direct rendezvous
/// path.
#[derive(Debug, Clone, Default)]
pub(crate) struct LookupSet {
    pub mdns: bool,
    pub dht: bool,
    pub relay: RelaySelection,
}

impl LookupSet {
    fn any(&self) -> bool {
        self.mdns || self.dht || self.relay.is_set()
    }
}

/// Resolve the CLI inputs into the effective [`LookupOpts`] baked into
/// the swarm id. Naming **any** lookup flag uses *only* those passed (so
/// `--mdns` alone is mDNS-only, relay/dht off); naming **none** but
/// passing `public` enables the all-on preset; naming nothing at all is
/// a loopback-only swarm. `--relay` bare ⇒ pinned default, `--relay
/// <url>` ⇒ custom ladder.
fn resolve_lookups(public: bool, lookups: LookupSet) -> LookupOpts {
    if lookups.any() {
        let relay = match lookups.relay {
            RelaySelection::Unset => RelayChoice::Disabled,
            RelaySelection::Default => RelayChoice::Pinned,
            RelaySelection::Custom(ladder) => RelayChoice::Custom(ladder.as_urls().to_vec()),
        };
        LookupOpts {
            mdns: lookups.mdns,
            dht: lookups.dht,
            relay,
        }
    } else if public {
        LookupOpts::public_preset()
    } else {
        LookupOpts::loopback()
    }
}

/// Resolve a transfer command's discovery config from its two alternative
/// sources: a `--swarm 🐝…` id (whose embedded lookups win) or the
/// create-style `--mdns/--dht/--relay` flags (naming any uses only
/// those). Naming **nothing** is the all-on public preset — a transfer is
/// inherently networked. The `--swarm`-vs-flags exclusivity is enforced
/// by clap; the both-sources bail below is a backstop for non-CLI
/// callers.
///
/// # Errors
/// Both sources given (ambiguous), or an invalid `--swarm` id.
pub(crate) fn resolve_transfer_lookups(
    swarm: Option<&str>,
    flags: LookupSet,
) -> Result<LookupOpts> {
    match swarm {
        Some(id) => {
            if flags.any() {
                bail!(
                    "--swarm already carries a discovery config; \
                     drop the --mdns/--dht/--relay flags"
                );
            }
            super::swarm_id_lookups(id).context("invalid --swarm id")
        }
        None => Ok(resolve_lookups(true, flags)),
    }
}

/// Parse a comma-separated, ordered relay **ladder** (`a,b,c`) — order
/// preserved; an empty or whitespace-only entry is a hard error so a typo
/// never silently shrinks the ladder. The single source of truth for
/// `--relay` syntax; `String` error so clap can surface it directly.
fn parse_relay_ladder(raw: &str) -> Result<Vec<RelayUrl>, String> {
    raw.split(',')
        .map(|entry| {
            let trimmed = entry.trim();
            if trimmed.is_empty() {
                return Err(format!("empty entry in relay ladder {raw:?}"));
            }
            trimmed
                .parse::<RelayUrl>()
                .map_err(|error| format!("invalid relay URL {trimmed:?}: {error}"))
        })
        .collect()
}

/// An ordered, non-empty relay ladder (`a,b,c` in preference order),
/// validated at construction. Parsing reuses `parse_relay_ladder` — the
/// same source of truth as the CLI value parser — and rejects empty
/// entries, so a `RelayLadder` is never empty; "no custom ladder" is the
/// `Option::None` case at the boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RelayLadder(Vec<RelayUrl>);

/// A relay ladder that couldn't be parsed (empty entry / invalid URL).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RelayLadderError(String);

impl fmt::Display for RelayLadderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for RelayLadderError {}

impl FromStr for RelayLadder {
    type Err = RelayLadderError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        parse_relay_ladder(input)
            .map(RelayLadder)
            .map_err(RelayLadderError)
    }
}

impl RelayLadder {
    /// The ordered rungs, for internal consumers.
    pub(crate) fn as_urls(&self) -> &[RelayUrl] {
        &self.0
    }
}

impl fmt::Display for RelayLadder {
    /// The canonical `a,b,c` text form — round-trips through [`FromStr`].
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, url) in self.0.iter().enumerate() {
            if index > 0 {
                formatter.write_str(",")?;
            }
            write!(formatter, "{url}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod lookup_tests {
    use super::{
        LookupOpts, LookupSet, RelayChoice, RelayLadder, RelaySelection, resolve_lookups,
        resolve_transfer_lookups,
    };

    fn lookups(mdns: bool, dht: bool, relay: RelaySelection) -> LookupSet {
        LookupSet { mdns, dht, relay }
    }

    #[test]
    fn relay_ladder_parses_ordered_rungs() {
        let one: RelayLadder = "https://a.example".parse().unwrap();
        assert_eq!(one.as_urls().len(), 1);

        let two: RelayLadder = "https://a.example,https://b.example".parse().unwrap();
        assert_eq!(two.as_urls().len(), 2);
        // Display round-trips through FromStr (canonical `a,b` text form).
        let rendered = two.to_string();
        assert_eq!(rendered.parse::<RelayLadder>().unwrap(), two);
    }

    #[test]
    fn relay_ladder_rejects_empty_and_empty_entries() {
        assert!("".parse::<RelayLadder>().is_err());
        assert!(
            "https://a.example,,https://b.example"
                .parse::<RelayLadder>()
                .is_err(),
            "an empty entry must be rejected so a typo never shrinks the ladder"
        );
    }

    #[test]
    fn naming_relay_enables_it_without_public() {
        // Granular model: naming any lookup uses only those, regardless of
        // `public`. A relay alone yields a reachable (non-loopback) swarm.
        let ladder: RelayLadder = "https://a.example".parse().unwrap();
        let opts = resolve_lookups(false, lookups(false, false, RelaySelection::Custom(ladder)));
        assert!(!opts.mdns && !opts.dht);
        assert!(
            !opts.is_loopback(),
            "a named relay makes the swarm reachable"
        );
        assert!(matches!(opts.relay, RelayChoice::Custom(_)));
    }

    #[test]
    fn public_no_flags_enables_all_three() {
        let opts = resolve_lookups(true, LookupSet::default());
        assert!(opts.mdns && opts.dht);
        assert_eq!(opts.relay, RelayChoice::Pinned, "preset ⇒ pinned relay");
        assert!(!opts.is_loopback());
    }

    #[test]
    fn no_public_no_flags_is_loopback() {
        let opts = resolve_lookups(false, LookupSet::default());
        assert!(opts.is_loopback());
        assert_eq!(opts.network_label(), "private");
    }

    #[test]
    fn mdns_alone_disables_dht_and_relay() {
        let opts = resolve_lookups(false, lookups(true, false, RelaySelection::Unset));
        assert!(opts.mdns && !opts.dht);
        assert_eq!(
            opts.relay,
            RelayChoice::Disabled,
            "--mdns alone ⇒ relay off"
        );
        assert!(!opts.is_loopback(), "any lookup ⇒ reachable");
    }

    #[test]
    fn bare_relay_is_pinned_and_suppresses_lookups() {
        let opts = resolve_lookups(false, lookups(false, false, RelaySelection::Default));
        assert!(!opts.mdns && !opts.dht);
        assert_eq!(opts.relay, RelayChoice::Pinned);
    }

    #[test]
    fn valued_relay_preserves_ladder_order() {
        let rung0: iroh::RelayUrl = "https://a.example".parse().unwrap();
        let rung1: iroh::RelayUrl = "https://b.example".parse().unwrap();
        let ladder: RelayLadder = "https://a.example,https://b.example".parse().unwrap();
        let opts = resolve_lookups(false, lookups(false, false, RelaySelection::Custom(ladder)));
        assert_eq!(opts.relay, RelayChoice::Custom(vec![rung0, rung1]));
    }

    #[test]
    fn transfer_no_flags_is_the_public_preset() {
        let opts = resolve_transfer_lookups(None, LookupSet::default()).unwrap();
        assert_eq!(opts, LookupOpts::public_preset());
    }

    #[test]
    fn transfer_named_flags_restrict_to_those() {
        let opts =
            resolve_transfer_lookups(None, lookups(true, false, RelaySelection::Unset)).unwrap();
        assert!(opts.mdns && !opts.dht);
        assert_eq!(opts.relay, RelayChoice::Disabled);
    }

    #[test]
    fn transfer_swarm_id_wins_and_rejects_flags() {
        let id = crate::protocol::swarm::encode_test_swarm_id("test", &LookupOpts::loopback());
        // The id's embedded lookups win when no flag is passed.
        let opts = resolve_transfer_lookups(Some(&id), LookupSet::default()).unwrap();
        assert!(opts.is_loopback());
        // Both sources at once is ambiguous (clap rejects it first on the CLI).
        let error =
            resolve_transfer_lookups(Some(&id), lookups(true, false, RelaySelection::Unset))
                .unwrap_err();
        assert!(error.to_string().contains("--swarm"), "got: {error}");
    }
}
