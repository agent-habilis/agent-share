use xshell::{Shell, cmd};

use crate::TaskOutcome;
use crate::util::output;

pub(crate) fn run(sh: &Shell) -> TaskOutcome {
    output::status("Installing", "agent-share");
    // `--force` is required: the crate version rarely changes between
    // builds, and without `--force`
    // `cargo install --path .` treats "already installed" as up-to-date
    // and **skips the rebuild entirely**, silently leaving the previously
    // installed binary in place — that shipped a stale binary before. `--force`
    // always rebuilds + reinstalls the current tree.
    //
    // `--locked` is equally load-bearing: without it `cargo install`
    // ignores Cargo.lock and freshly resolves on the host, so a registry
    // release after the lock was cut can change the build.
    cmd!(sh, "cargo install --path . --force --locked")
        .quiet()
        .run()?;
    output::status("Installed", "~/.cargo/bin/agent-share");
    Ok(())
}
