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

    // The web app is half the product, and the gate had never looked at it:
    // 404 `bun test` cases and five `tsc` projects, none of them run here.
    //
    // Skipped with a message rather than failed when bun is absent, the same
    // rule the wasm blocks below follow — a gate that fails for a reason
    // unrelated to the change is a gate people learn to skip.
    output::status("Checking", "the web app");
    if cmd!(sh, "bun --version")
        .quiet()
        .ignore_status()
        .read()
        .is_ok()
    {
        let _guard = sh.push_dir("web");
        // Only on a cold checkout: installing every run would put the network
        // on the critical path of a gate that otherwise needs none.
        if !sh.path_exists("node_modules") {
            cmd!(sh, "bun install --frozen-lockfile").quiet().run()?;
        }
        cmd!(sh, "bun run typecheck").quiet().run()?;
        cmd!(sh, "bun test").quiet().run()?;
    } else {
        output::status("Skipping", "the web app (bun is not installed)");
    }

    // The transport id and the signal envelope must have exactly one
    // definition each. They were duplicated across two crates once already,
    // kept in sync by a comment; peers that disagree on either fail to connect
    // with no useful error, so the de-duplication is worth a test.
    //
    // Their one definition now lives in `fofoca-iroh-webrtc-transport`, which
    // moved to the `fofoca-network/fofoca` workspace — so what this side can
    // still assert is the half that matters here: neither is redeclared
    // locally. A copy in this tree is exactly the drift the original check
    // existed to catch, and it would compile.
    output::status("Checking", "no redeclared wire constants");
    for needle in ["0x5752_5443", "enum SignalEnvelope"] {
        // Source files only: a README is free to name the constant in prose.
        let hits = cmd!(sh, "grep -rl --include=*.rs {needle} crates")
            .quiet()
            .ignore_status()
            .read()?;
        let files: Vec<_> = hits.lines().filter(|line| !line.is_empty()).collect();
        if !files.is_empty() {
            return Err(format!(
                "`{needle}` is owned by fofoca-iroh-webrtc-transport and must not be \
                 redeclared here, found in {files:?}"
            )
            .into());
        }
    }

    // The wire format and the browser transport exist to be linked by the
    // browser client, so a host-only dependency sneaking in must fail here
    // rather than surface later as a wasm-pack error nobody connects back to
    // the offending crate.
    output::status("Checking", "wasm32 targets");
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
        // The engine's own wasm32 legs — a portable check and the
        // `wasm_runtime` suite — moved with it to `fofoca-network/fofoca` and
        // run in that repo's CI. What is left here is this repo's own code.
        //
        // The wasm client is excluded from the workspace, so nothing above
        // reaches it — and its tests run *here*, on wasm32, rather than with
        // the other `cargo test` lines above.
        //
        // Not a preference. Off wasm32 `fofoca` turns on
        // `fofoca-iroh-webrtc-transport/native`, and with both backends enabled
        // `WebRtcHandle` resolves to the host one while this crate hands it a
        // `BrowserHubTransport` — so a host build cannot type-check by
        // construction. CI ran it on the host anyway and had been red for it.
        if let Some(clang) = wasm_clang(sh) {
            let _guard = sh.push_dir("crates/agent-share-wasm-client");
            for args in [
                "check --target wasm32-unknown-unknown",
                "test --target wasm32-unknown-unknown --lib",
            ] {
                let args = args.split(' ');
                cmd!(sh, "cargo {args...}")
                    .env("CC", &clang)
                    .env("CC_wasm32_unknown_unknown", &clang)
                    .quiet()
                    .run()?;
            }
        }
    } else {
        output::status(
            "Skipping",
            "wasm32 checks (rustup target add wasm32-unknown-unknown)",
        );
    }

    crate::util::sweep_stale_artifacts(sh);
    Ok(())
}

/// A clang that can emit `wasm32`, or `None`.
///
/// Apple clang ships no wasm backend, so `ring`'s C core fails to build with
/// the default `cc` on macOS. Homebrew LLVM does have one. Returns `None`
/// rather than guessing so the caller can skip with a message instead of
/// failing a run on an unrelated host.
pub(crate) fn wasm_clang(sh: &Shell) -> Option<String> {
    for candidate in [
        "/opt/homebrew/opt/llvm/bin/clang",
        "/usr/local/opt/llvm/bin/clang",
    ] {
        if sh.path_exists(candidate) {
            return Some(candidate.to_owned());
        }
    }
    // A non-Apple clang on PATH (most Linux hosts) targets wasm32 fine.
    let version = cmd!(sh, "clang --version")
        .quiet()
        .ignore_status()
        .read()
        .ok()?;
    (!version.contains("Apple clang")).then(|| "clang".to_owned())
}
