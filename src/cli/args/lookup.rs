use clap::Parser;

use crate::protocol::swarm::{LookupSet, RelayLadder, RelaySelection};

/// The lookup allowlist flags: naming any uses *only* those passed (so
/// `--mdns` alone disables both dht and the relay); naming none falls
/// back to the all-on public preset. Grouped and flattened so each
/// options struct stays within the readable bool budget.
#[derive(Parser, Debug)]
pub(crate) struct LookupArgs {
    /// Enable the LAN mDNS address-lookup.
    #[arg(long, default_value_t = false)]
    pub mdns: bool,

    /// Enable the mainline-DHT address-lookup.
    #[arg(long, default_value_t = false)]
    pub dht: bool,

    /// Enable the relay (connectivity + relay-direct rendezvous). Bare
    /// `--relay` ⇒ the default n0 prod relay *ladder*; `--relay
    /// <URL>[,<URL>…]` ⇒ a custom ordered ladder. Omitting it while
    /// naming another flag disables the relay; naming no flag at all
    /// falls back to the public preset. Absent ⇒ `None`; bare ⇒
    /// `Some(None)`; valued ⇒ `Some(Some(ladder))`.
    #[arg(long, num_args(0..=1))]
    #[expect(
        clippy::option_option,
        reason = "clap optional-value flag: absent/bare/valued are three distinct relay states (see RelaySelection)"
    )]
    pub relay: Option<Option<RelayLadder>>,
}

impl LookupArgs {
    pub(crate) fn to_set(&self) -> LookupSet {
        let relay = match &self.relay {
            None => RelaySelection::Unset,
            Some(None) => RelaySelection::Default,
            Some(Some(ladder)) => RelaySelection::Custom(ladder.clone()),
        };
        LookupSet {
            mdns: self.mdns,
            dht: self.dht,
            relay,
        }
    }
}

/// [`LookupArgs`] plus `--public` — the no-flag default for a transfer is
/// already the all-on public preset; `--public` is its explicit alias.
#[derive(Parser, Debug)]
pub(crate) struct PublicLookupArgs {
    /// Explicitly select the all-on public preset (mDNS + DHT + the
    /// default relay ladder) — already the default when no lookup flag
    /// is named. Conflicts with the granular `--mdns`/`--dht`/`--relay`
    /// flags (they replace the preset) and with a `--swarm` that already
    /// carries a discovery config.
    #[arg(long, default_value_t = false, conflicts_with_all = ["mdns", "dht", "relay"])]
    pub public: bool,

    #[command(flatten)]
    pub lookups: LookupArgs,
}

impl PublicLookupArgs {
    pub(crate) fn to_set(&self) -> LookupSet {
        if self.public {
            // The explicit alias for the no-flag default; the conflict
            // rule guarantees no granular flag accompanies it.
            LookupSet::default()
        } else {
            self.lookups.to_set()
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::cli::args::{Cli, MountAction};
    use crate::protocol::swarm::RelaySelection;

    /// Parse `agent-share serve …` and read the resolved relay selection —
    /// the `--relay` allowlist flag lives in [`super::LookupArgs`],
    /// exercised here through the serve subcommand.
    fn relay_of(args: &[&str]) -> RelaySelection {
        let Some(MountAction::Serve { lookups, .. }) = Cli::parse_from(args).action else {
            panic!("expected Serve");
        };
        lookups.to_set().relay
    }

    #[test]
    fn relay_flag_absent_bare_and_valued() {
        assert_eq!(
            relay_of(&["agent-share", "serve", "./dir"]),
            RelaySelection::Unset,
            "absent ⇒ Unset"
        );
        assert_eq!(
            relay_of(&["agent-share", "serve", "./dir", "--relay"]),
            RelaySelection::Default,
            "bare ⇒ Default (pinned)"
        );
        assert_eq!(
            relay_of(&[
                "agent-share",
                "serve",
                "./dir",
                "--relay",
                "https://relay.example"
            ]),
            RelaySelection::Custom("https://relay.example".parse().unwrap()),
            "valued ⇒ single-rung Custom ladder"
        );
        assert_eq!(
            relay_of(&[
                "agent-share",
                "serve",
                "./dir",
                "--relay",
                "https://a.example,https://b.example"
            ]),
            RelaySelection::Custom("https://a.example,https://b.example".parse().unwrap()),
            "comma-separated ⇒ ordered multi-rung ladder"
        );
    }

    #[test]
    fn relay_flag_rejects_empty_ladder_entry() {
        let parsed = Cli::try_parse_from([
            "agent-share",
            "serve",
            "./dir",
            "--relay",
            "https://a.example,,https://b.example",
        ]);
        assert!(parsed.is_err(), "empty entry must be rejected");
    }
}
