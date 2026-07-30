use xshell::{Shell, cmd};

use crate::TaskOutcome;
use crate::util::output;

pub(crate) fn run(sh: &Shell) -> TaskOutcome {
    output::status("Checking", "formatting");
    cmd!(sh, "cargo fmt --check").quiet().run()?;

    output::status("Running", "clippy");
    cmd!(sh, "cargo clippy --workspace --all-targets -- -D warnings")
        .quiet()
        .run()?;

    output::status("Running", "tests");
    cmd!(sh, "cargo test --workspace").quiet().run()?;

    // `agent-share-proto` is the one implementation of the wire format that
    // the CLI producer, the CLI consumer, and the browser client all link.
    // Its whole value is that it builds for the browser too, so a host-only
    // dependency sneaking in must fail CI rather than surface later as a
    // wasm-pack error nobody connects back to this crate.
    output::status("Checking", "agent-share-proto on wasm32");
    if cmd!(sh, "rustup target list --installed")
        .quiet()
        .read()?
        .lines()
        .any(|target| target == "wasm32-unknown-unknown")
    {
        cmd!(
            sh,
            "cargo check --target wasm32-unknown-unknown -p agent-share-proto"
        )
        .quiet()
        .run()?;
    } else {
        output::status(
            "Skipping",
            "wasm32 check (rustup target add wasm32-unknown-unknown)",
        );
    }

    crate::util::sweep_stale_artifacts(sh);
    Ok(())
}
