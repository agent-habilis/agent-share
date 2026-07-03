//! Thin binary shim. All CLI logic lives in the library
//! ([`ahmo::run_cli`]); `main` owns only process-level concerns the
//! library must not: tracing init.

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();
    ahmo::run_cli().await
}
