//! Dispatch: the direct port of the `mount()` handler from
//! agent-habilis/swarm's `src/cli/mod.rs`, with the `Mount` subcommand
//! hoisted to the root command.

pub(crate) mod args;

use anyhow::Result;

use args::{Cli, MountAction, OutputFormat};

pub(crate) async fn run(cli: Cli) -> Result<()> {
    let json = matches!(cli.output, OutputFormat::Json);
    if let Some(MountAction::Serve {
        dir,
        swarm,
        lookups,
        output: serve_output,
    }) = cli.action
    {
        return crate::mount::serve(
            swarm.as_ref().map(crate::protocol::SwarmId::as_str),
            lookups.to_set(),
            &dir,
            matches!(serve_output, OutputFormat::Json),
        )
        .await;
    }
    // The bare form: both positionals are optional at the clap layer (the
    // `serve` subcommand shares the slot), so require them here.
    let (Some(ticket), Some(mountpoint)) = (cli.ticket, cli.mountpoint) else {
        anyhow::bail!("usage: agent-share <🐝…> <mountpoint>, or agent-share serve <dir>");
    };
    crate::mount::attach(&ticket, &mountpoint, cli.no_mount, json).await
}
