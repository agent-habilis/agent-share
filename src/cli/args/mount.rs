use std::path::PathBuf;

use clap::Subcommand;

use super::lookup::PublicLookupArgs;
use super::output::OutputFormat;
use crate::protocol::SwarmId;

/// The `ahmo serve` action. The consumer side is the bare
/// `ahmo <🐝…> <mountpoint>` form (positionals on the root command),
/// so a `🐝…` ticket can never collide with the `serve` literal.
#[derive(Subcommand, Debug)]
pub(crate) enum MountAction {
    /// Share a folder read-only; prints the `ahmo 🐝…` command on stdout.
    ///
    /// Lazy: the tree is scanned once at startup (a metadata-only snapshot —
    /// nothing is hashed or transferred up front); peers fetch file bytes on
    /// demand as they read them. Changes after startup are not visible until
    /// you restart `serve` and peers remount. Keeps serving until interrupted.
    Serve {
        /// The directory to share.
        dir: PathBuf,
        /// Swarm id whose discovery config (local / mDNS / DHT / relay) the
        /// share should use, so it traverses the network like swarm members
        /// do. Omit for a public default. Alternative to the
        /// `--mdns`/`--dht`/`--relay` flags — pass one or the other.
        #[arg(long, conflicts_with_all = ["public", "mdns", "dht", "relay"])]
        swarm: Option<SwarmId>,
        /// Which lookup mechanisms the share uses: naming any uses only
        /// those; naming none (or `--public`) is the all-on public preset.
        #[command(flatten)]
        lookups: PublicLookupArgs,
        /// Output format: human (default) — a cargo-style status + hint — or
        /// json, a single direct `ahmo 🐝…` line for machines.
        #[arg(long, default_value = "human")]
        output: OutputFormat,
    },
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::cli::args::Cli;

    #[test]
    fn mount_serve_parses() {
        let cli = Cli::parse_from(["ahmo", "serve", "./dir"]);
        let Some(super::MountAction::Serve { dir, swarm, .. }) = cli.action else {
            panic!("expected Serve");
        };
        assert_eq!(dir, std::path::PathBuf::from("./dir"));
        assert!(swarm.is_none());
    }

    #[test]
    fn mount_ticket_form_parses() {
        let cli = Cli::parse_from(["ahmo", "🐝abc", "./mnt", "--output", "json"]);
        assert!(cli.action.is_none());
        assert_eq!(cli.ticket.as_deref(), Some("🐝abc"));
        assert_eq!(cli.mountpoint, Some(std::path::PathBuf::from("./mnt")));
        assert!(!cli.no_mount);
        assert!(matches!(cli.output, super::OutputFormat::Json));
    }

    #[test]
    fn mount_serve_parses_lookup_flags() {
        let cli = Cli::parse_from(["ahmo", "serve", "./dir", "--dht"]);
        let Some(super::MountAction::Serve { lookups, .. }) = cli.action else {
            panic!("expected Serve");
        };
        assert!(!lookups.public);
        assert!(lookups.lookups.dht && !lookups.lookups.mdns);
    }

    #[test]
    fn mount_serve_public_conflicts_with_granular_and_swarm() {
        assert!(
            Cli::try_parse_from(["ahmo", "serve", "./dir", "--public", "--dht"]).is_err()
        );
        let id = crate::protocol::swarm::encode_test_swarm_id(
            "test",
            &crate::protocol::swarm::LookupOpts::loopback(),
        );
        assert!(
            Cli::try_parse_from([
                "ahmo",
                "serve",
                "./dir",
                "--swarm",
                id.as_str(),
                "--public"
            ])
            .is_err()
        );
    }

    #[test]
    fn mount_without_args_is_rejected_at_dispatch_not_parse() {
        // Both positionals are optional at the clap layer (the serve
        // subcommand shares the slot); the handler errors with usage.
        let cli = Cli::parse_from(["ahmo"]);
        assert!(cli.action.is_none());
        assert!(cli.ticket.is_none());
    }
}
