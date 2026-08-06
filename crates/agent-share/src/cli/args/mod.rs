//! The clap surface. The root command is what `ahsw mount` was in
//! agent-habilis/swarm, hoisted to a standalone binary: the bare
//! `agent-share <ticket> <target>` consumer form plus the `serve`
//! subcommand.

use std::path::PathBuf;

use clap::Parser;

mod lookup;
mod mount;
mod output;
mod password;

pub(crate) use mount::MountAction;
pub(crate) use output::OutputFormat;
pub(crate) use password::PasswordArgs;

/// Share a folder with peers, or mount a peer's folder locally
/// (read-only, lazy, no daemon).
///
/// `agent-share serve <dir>` shares a folder and prints the `agent-share <ticket>`
/// command; `agent-share <ticket> <target>` creates `agent-share-…/` under the
/// target and mounts through a loopback `NFSv3` bridge (the OS's built-in NFS
/// client — no FUSE, no kernel extension). File bytes are fetched on demand
/// as they are read. Writes fail (read-only).
#[derive(Parser, Debug)]
#[command(name = "agent-share", version, args_conflicts_with_subcommands = true)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub action: Option<MountAction>,

    /// The ticket printed by `agent-share serve`.
    pub ticket: Option<String>,

    /// Parent directory for the mount. Creates `agent-share-YYYY-MM-DDTHHMM/`
    /// inside it (target may be non-empty). Unmounted on Ctrl-C.
    pub mountpoint: Option<PathBuf>,

    /// Start the loopback NFS bridge but skip the OS mount step; prints
    /// the mount command to run manually. Hidden — a test/ops knob.
    #[arg(long, hide = true)]
    pub no_mount: bool,

    /// Mount data path: `webrtc` forces the data channel and fails if the
    /// mount settles anywhere else. Omit for the default, which prefers iroh's
    /// own hole-punched paths and falls to `WebRTC` only after the discovery
    /// deadline.
    #[arg(long)]
    pub transport: Option<String>,

    /// Password for a protected share. Required when the ticket says the share
    /// carries one — the ticket alone will not open it.
    #[command(flatten)]
    pub password: PasswordArgs,

    /// Output format: human (default) or json (the bare mount command).
    #[arg(long, default_value = "human")]
    pub output: OutputFormat,
}
