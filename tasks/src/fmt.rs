use xshell::{Shell, cmd};

use crate::TaskOutcome;
use crate::util::repo_root;

// `rustfmt.toml` sets `group_imports`, which only nightly rustfmt honours.
// Pinned by date because unstable rustfmt options change between nightlies.
const NIGHTLY: &str = "+nightly-2026-06-05";

pub(crate) fn run(sh: &Shell) -> TaskOutcome {
    fmt(sh, &[])
}

pub(crate) fn check(sh: &Shell) -> TaskOutcome {
    fmt(sh, &["--check"])
}

fn fmt(sh: &Shell, args: &[&str]) -> TaskOutcome {
    // The wasm client is excluded from the workspace, so a root `cargo fmt`
    // never reaches it.
    for dir in [".", "crates/agent-share-wasm-client"] {
        let _guard = sh.push_dir(repo_root().join(dir));
        cmd!(sh, "cargo {NIGHTLY} fmt {args...}").quiet().run()?;
    }
    Ok(())
}
