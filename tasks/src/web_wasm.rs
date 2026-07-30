//! Build `crates/agent-share-wasm-client/` into the two wasm-bindgen outputs
//! the front ends consume: `dist/web/` for the browser UI and `dist/nodejs/`
//! for the npx CLI.
//!
//! One `.wasm`, two glue layers. The crate is a standalone workspace, so this
//! shells into it rather than building from the root.
//!
//! Never part of `ci`: it needs the wasm target, the `wasm-bindgen` CLI, and a
//! wasm-capable clang (ring's C core — Apple clang cannot target wasm32,
//! Homebrew LLVM can). `ci` runs a plain `cargo check` for the same crates,
//! which catches the interesting breakage without those prerequisites.

use xshell::{Shell, cmd};

use crate::TaskOutcome;
use crate::util::output;

const WASM_TARGET: &str = "wasm32-unknown-unknown";
/// The wasm client lives under `crates/` but is deliberately excluded from the
/// workspace, so every command here shells into it by path.
const CRATE: &str = "crates/agent-share-wasm-client";
const ARTIFACT: &str = "target/wasm32-unknown-unknown/release/agent_share_wasm_client.wasm";

pub(crate) fn run(sh: &Shell) -> TaskOutcome {
    ensure_prereqs(sh)?;

    output::status("Building", "agent-share-wasm-client (wasm32, release)");
    {
        let _guard = sh.push_dir("crates/agent-share-wasm-client");
        let mut cargo = cmd!(sh, "cargo build --release --target {WASM_TARGET}").quiet();
        if let Some(clang) = crate::ci::wasm_clang(sh) {
            cargo = cargo
                .env("CC", &clang)
                .env(format!("CC_{WASM_TARGET}"), &clang);
        }
        cargo.run()?;
    }

    // `--target web` for the browser (ES modules, fetch-based wasm load) and
    // `--target nodejs` for the CLI (CommonJS, fs-based load). The same binary
    // either way; only the glue differs, so they cannot drift.
    for target in ["web", "nodejs"] {
        output::status("Bindgen", &format!("{CRATE}/dist/{target}"));
        cmd!(
            sh,
            "wasm-bindgen --target {target} --out-dir {CRATE}/dist/{target} {CRATE}/{ARTIFACT}"
        )
        .quiet()
        .run()?;
    }

    output::status("Finished", &format!("{CRATE}/dist/{{web,nodejs}}"));
    Ok(())
}

fn ensure_prereqs(sh: &Shell) -> TaskOutcome {
    let installed = cmd!(sh, "rustup target list --installed").quiet().read()?;
    if !installed.lines().any(|line| line == WASM_TARGET) {
        return Err(format!(
            "the {WASM_TARGET} target is missing — run `rustup target add {WASM_TARGET}`"
        )
        .into());
    }
    if cmd!(sh, "wasm-bindgen --version").quiet().read().is_err() {
        return Err(
            "the wasm-bindgen CLI is missing — run `cargo install wasm-bindgen-cli`".into(),
        );
    }
    Ok(())
}
