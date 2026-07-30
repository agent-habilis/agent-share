//! `agent-share` — share a directory read-only over iroh QUIC, consumed
//! locally through a loopback `NFSv3` mount. Extracted from
//! agent-habilis/swarm's `ahsw mount` and since forked to its own ALPN, so
//! both ends of a mount must run `agent-share`.
//!
//! Ships as a binary plus a minimal library surface: the lib exists so
//! the task runner (`cargo task man`) can walk the clap tree in-process.

use anyhow::Result;
use clap::{CommandFactory, Parser};

pub(crate) mod cli;
pub(crate) mod file;
pub(crate) mod lookup;
pub(crate) mod mount;
pub(crate) mod protocol;
pub(crate) mod util;

/// Parse argv and run the CLI end-to-end.
///
/// # Errors
/// Propagates any error from the selected subcommand.
pub async fn run_cli() -> Result<()> {
    cli::run(cli::args::Cli::parse()).await
}

/// Producer internals reached by the integration tests.
///
/// Not part of the CLI's contract and not intended for outside use: the
/// end-to-end WebRTC test drives a producer directly, and the alternative —
/// making `scan`, `ServedFile` and the mount server `pub` on the real module
/// paths — would widen the crate's surface for every consumer to buy one
/// test.
#[doc(hidden)]
pub mod test_support {
    pub use crate::lookup::add_peer_addr;
    pub use crate::mount::test_support::{scan, serve_mount, served_files};
}

/// The fully-built `agent-share` clap command tree, for offline man-page
/// generation (`cargo task man`). Arg surface only; no runtime state.
#[must_use]
pub fn cli_command() -> clap::Command {
    cli::args::Cli::command()
}
