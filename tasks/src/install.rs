use xshell::{Shell, cmd};

use crate::TaskOutcome;
use crate::util::output;

/// Where the installable package lives.
///
/// `cargo install` needs a *package* manifest and the workspace root is
/// virtual, so `--path .` fails with "found a virtual manifest ... instead of
/// a package manifest". `default-members` does not help — it steers
/// build/test/run, not `install`.
const PACKAGE: &str = "crates/agent-share";

pub(crate) fn run(sh: &Shell) -> TaskOutcome {
    output::status("Installing", "agent-share");
    // `--force` is required: the crate version rarely changes between
    // builds, and without `--force`
    // `cargo install` treats "already installed" as up-to-date
    // and **skips the rebuild entirely**, silently leaving the previously
    // installed binary in place — that shipped a stale binary before. `--force`
    // always rebuilds + reinstalls the current tree.
    //
    // `--locked` is equally load-bearing: without it `cargo install`
    // ignores Cargo.lock and freshly resolves on the host, so a registry
    // release after the lock was cut can change the build.
    cmd!(sh, "cargo install --path {PACKAGE} --force --locked")
        .quiet()
        .run()?;
    output::status("Installed", "~/.cargo/bin/agent-share");
    Ok(())
}
