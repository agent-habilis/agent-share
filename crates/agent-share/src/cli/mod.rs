//! Dispatch: the direct port of the `mount()` handler from
//! agent-habilis/swarm's `src/cli/mod.rs`, with the `Mount` subcommand
//! hoisted to the root command.

pub(crate) mod args;

use anyhow::Result;

use args::{Cli, MountAction, OutputFormat};

pub(crate) async fn run(cli: Cli) -> Result<()> {
    let json = matches!(cli.output, OutputFormat::Json);
    match cli.action {
        Some(MountAction::Serve {
            dir,
            swarm,
            lookups,
            output: serve_output,
        }) => {
            return crate::mount::serve(
                swarm.as_ref().map(crate::protocol::SwarmId::as_str),
                lookups.to_set(),
                &dir,
                matches!(serve_output, OutputFormat::Json),
            )
            .await;
        }
        Some(MountAction::Bench {
            ticket: None,
            transport,
            output: bench_output,
            ..
        }) => {
            let Some(transport) = transport else {
                anyhow::bail!("bench producer requires --transport webrtc|relay");
            };
            return crate::mount::produce_bench(
                &transport,
                matches!(bench_output, OutputFormat::Json),
            )
            .await;
        }
        Some(MountAction::Bench {
            ticket: Some(ticket),
            transport,
            duration,
            output: bench_output,
        }) => {
            if transport.is_some() {
                anyhow::bail!(
                    "bench consumer has no --transport; the producer sets it in the ticket"
                );
            }
            return crate::mount::run_bench(
                &ticket,
                duration,
                matches!(bench_output, OutputFormat::Json),
            )
            .await;
        }
        None => {}
    }
    // The bare form: both positionals are optional at the clap layer (the
    // `serve` subcommand shares the slot), so require them here.
    let (Some(ticket), Some(mountpoint)) = (cli.ticket, cli.mountpoint) else {
        anyhow::bail!(
            "usage: agent-share <🐝…> <target>, agent-share serve <dir>, or agent-share bench"
        );
    };
    crate::mount::attach(&ticket, &mountpoint, cli.no_mount, json).await
}
