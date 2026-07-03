//! `ahmo` — share a directory read-only over iroh QUIC, consumed
//! locally through a loopback `NFSv3` mount. Extracted from
//! agent-habilis/swarm's `ahsw mount`; tickets interoperate with it.

use anyhow::Result;
use clap::Parser;

mod cli;
mod file;
mod lookup;
mod mount;
mod protocol;
mod util;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();
    let args = cli::args::Cli::parse();
    cli::run(args).await
}
