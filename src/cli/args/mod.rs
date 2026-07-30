//! The clap surface. The root command is what `ahsw mount` was in
//! agent-habilis/swarm, hoisted to a standalone binary: the bare
//! `agent-share <🐝…> <mountpoint>` consumer form plus the `serve`
//! subcommand.

use std::path::PathBuf;

use clap::Parser;

mod lookup;
mod mount;
mod output;

pub(crate) use mount::MountAction;
pub(crate) use output::OutputFormat;

/// Share a folder with peers, or mount a peer's folder locally
/// (read-only, lazy, no daemon).
///
/// `agent-share serve <dir>` shares a folder and prints the `agent-share 🐝…`
/// command; `agent-share <🐝…> <mountpoint>` mounts it through a loopback `NFSv3`
/// bridge (the OS's built-in NFS client — no FUSE, no kernel extension).
/// The directory tree is a snapshot from when `serve` started; file bytes
/// are fetched on demand as they are read. Writes fail (read-only).
#[derive(Parser, Debug)]
#[command(name = "agent-share", version, args_conflicts_with_subcommands = true)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub action: Option<MountAction>,

    /// The `🐝…` ticket printed by `agent-share serve`.
    pub ticket: Option<String>,

    /// Where to mount the shared folder (created if missing; an existing
    /// directory must be empty). Unmounted on Ctrl-C.
    pub mountpoint: Option<PathBuf>,

    /// Start the loopback NFS bridge but skip the OS mount step; prints
    /// the mount command to run manually. Hidden — a test/ops knob.
    #[arg(long, hide = true)]
    pub no_mount: bool,

    /// Output format: human (default) or json (the bare mount command).
    #[arg(long, default_value = "human")]
    pub output: OutputFormat,
}
