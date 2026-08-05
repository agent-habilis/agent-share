//! Turning `--mdns/--dht/--relay` into a [`LookupOpts`].
//!
//! The allowlist **types and their byte codec** live in
//! [`agent_share_proto::lookup`], because a ticket carries them and the
//! browser has to decode the same bytes. What stays here is the half a
//! browser has no use for: resolving CLI flags into an allowlist, and parsing
//! the `--relay a,b,c` ladder syntax.

use std::fmt;
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use fofoca::iroh::RelayUrl;

pub(crate) use agent_share_proto::lookup::{LookupOpts, RelayChoice, read_u16};

/// Relay intent from the CLI: absent / default / custom. Resolved into a
/// [`RelayChoice`] by [`resolve_lookups`]. `Custom` carries the ordered
/// [`RelayLadder`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum RelaySelection {
    /// No relay (the CLI `--relay` flag absent).
    #[default]
    Unset,
    /// The pinned default relay ladder (bare `--relay`).
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
/// sources: a `--swarm <id>` (whose embedded lookups win) or the
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
        let rung0: fofoca::iroh::RelayUrl = "https://a.example".parse().unwrap();
        let rung1: fofoca::iroh::RelayUrl = "https://b.example".parse().unwrap();
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
