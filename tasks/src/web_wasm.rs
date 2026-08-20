//! Build `crates/agent-share-wasm-client/` into the two wasm-bindgen outputs
//! the front ends consume, both inside `packages/agent-share-wasm/`:
//! `src/glue/` for the browser UI and `node/` for the npx CLI.
//!
//! The build itself moved to `scripts/build-wasm.ts`, so that `bun run
//! build` is self-contained rather than depending on this task having been run
//! by hand first. What is left here is the entry point the rest of the repo
//! already reaches for — `e2e`, the browser bench, the README — pointed at the
//! one implementation.
//!
//! Never part of `ci`: it needs the wasm target, the `wasm-bindgen` CLI, and a
//! wasm-capable clang (ring's C core — Apple clang cannot target wasm32,
//! Homebrew LLVM can). `ci` runs a plain `cargo check` for the same crates,
//! which catches the interesting breakage without those prerequisites.

use xshell::{Shell, cmd};

use crate::TaskOutcome;
use crate::util::{output, repo_root};

pub(crate) fn run(sh: &Shell) -> TaskOutcome {
    if cmd!(sh, "bun --version").quiet().read().is_err() {
        return Err("bun is missing — see https://bun.sh".into());
    }

    output::status("Building", "agent-share-wasm-client (wasm32, release)");
    // Anchored rather than cwd-relative, like `web_image` and the bench
    // harness: `cargo task` runs from wherever it was invoked, and the bun
    // workspace root is the repo root.
    let _guard = sh.push_dir(repo_root());
    cmd!(sh, "bun scripts/build-wasm.ts").run()?;

    output::status("Finished", "packages/agent-share-wasm/{src/glue,node}");
    Ok(())
}
