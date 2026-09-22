//! Dispatch: the direct port of the `mount()` handler from
//! agent-habilis/swarm's `src/cli/mod.rs`, with the `Mount` subcommand
//! hoisted to the root command.

use anyhow::Result;

use self::args::{Cli, MountAction, OutputFormat};

pub(crate) mod args;

/// Read the consumer's `--transport` flag.
///
/// Shared by the two consumer forms — the bare `agent-share <ticket> <target>`
/// mount and `mirror` — so a spelling the mount accepts cannot be one the mirror
/// rejects. `webrtc` is the only thing that turns the lane on; everything else
/// either names the default or is a usage error.
fn webrtc_only(flag: Option<&str>) -> Result<bool> {
    match flag.map(str::trim) {
        None | Some("") => Ok(false),
        Some(raw) => match raw.to_ascii_lowercase().as_str() {
            "webrtc" | "webrtc_only" | "webrtc-only" => Ok(true),
            "dynamic" | "default" => Ok(false),
            other => anyhow::bail!(
                "unknown transport {other:?}; expected webrtc (or omit for the default)"
            ),
        },
    }
}

pub(crate) async fn run(cli: Cli) -> Result<()> {
    let json = matches!(cli.output, OutputFormat::Json);
    match cli.action {
        Some(MountAction::Serve {
            dir,
            swarm,
            lookups,
            password,
            output: serve_output,
        }) => {
            return crate::mount::serve(
                swarm.as_ref().map(crate::protocol::SwarmId::as_str),
                lookups.to_set(),
                &dir,
                password.resolve()?.as_deref(),
                matches!(serve_output, OutputFormat::Json),
            )
            .await;
        }
        Some(MountAction::Mirror {
            ticket,
            dest,
            only,
            transport,
            password,
            output: mirror_output,
        }) => {
            return crate::mount::mirror(
                &ticket,
                &dest,
                &only,
                webrtc_only(transport.as_deref())?,
                password.resolve()?.as_deref(),
                matches!(mirror_output, OutputFormat::Json),
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
                anyhow::bail!("bench producer requires --transport webrtc|relay|quic");
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
            depth,
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
                depth,
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
            "usage: agent-share <ticket> <target>, agent-share serve <dir>, or agent-share bench"
        );
    };
    crate::mount::attach(
        &ticket,
        &mountpoint,
        cli.no_mount,
        json,
        webrtc_only(cli.transport.as_deref())?,
        cli.password.resolve()?.as_deref(),
    )
    .await
}
