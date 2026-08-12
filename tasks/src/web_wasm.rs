//! Build `crates/agent-share-wasm-client/` into the two wasm-bindgen outputs
//! the front ends consume: `dist/web/` for the browser UI and `dist/nodejs/`
//! for the npx CLI.
//!
//! The build itself moved to `web/scripts/build-wasm.ts`, so that `bun run
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
use crate::util::output;

pub(crate) fn run(sh: &Shell) -> TaskOutcome {
    if cmd!(sh, "bun --version").quiet().read().is_err() {
        return Err("bun is missing — see https://bun.sh".into());
    }

    output::status("Building", "agent-share-wasm-client (wasm32, release)");
    let _guard = sh.push_dir("web");
    cmd!(sh, "bun scripts/build-wasm.ts").run()?;

    output::status(
        "Finished",
        "crates/agent-share-wasm-client/dist/{web,nodejs}",
    );
    Ok(())
}
