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
    // The host backend is not in the default feature set, so a plain
    // `--workspace` run never touches it.
    cmd!(
        sh,
        "cargo test -p fofoca-iroh-webrtc-transport --features host"
    )
    .quiet()
    .run()?;
    // Standalone workspace (excluded from the root); covers transport-mode
    // parsing for ShareClient::connect.
    cmd!(
        sh,
        "cargo test --manifest-path crates/agent-share-wasm-client/Cargo.toml --lib"
    )
    .quiet()
    .run()?;

    // The transport id and the signal envelope must have exactly one
    // definition each. They were duplicated across two crates upstream, kept
    // in sync by a comment; peers that disagree on either fail to connect with
    // no useful error, so the de-duplication is worth a test.
    output::status("Checking", "no duplicated wire constants");
    for (needle, owner) in [
        (
            "0x5752_5443",
            "crates/fofoca-iroh-webrtc-transport/src/addr.rs",
        ),
        (
            "enum SignalEnvelope",
            "crates/fofoca-iroh-webrtc-transport/src/signaling.rs",
        ),
    ] {
        // Source files only: a README is free to name the constant in prose,
        // and this crate's does precisely to explain the rule.
        let hits = cmd!(sh, "grep -rl --include=*.rs {needle} crates")
            .quiet()
            .ignore_status()
            .read()?;
        let files: Vec<_> = hits.lines().filter(|line| !line.is_empty()).collect();
        if files != [owner] {
            return Err(
                format!("`{needle}` must be defined only in {owner}, found in {files:?}").into(),
            );
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
        // `ring`'s C core needs a wasm-capable clang; Apple clang cannot
        // target wasm32. Skip rather than fail on a host without one — the
        // proto check above still guards the wire format.
        match wasm_clang(sh) {
            Some(clang) => {
                for args in [
                    "check --target wasm32-unknown-unknown -p fofoca-iroh-webrtc-transport --features web",
                    // The engine itself must reach the browser, not merely be
                    // avoidable from it. Without this gate the wasm target rots
                    // on the next edit that reaches for a file or a socket.
                    "check --target wasm32-unknown-unknown -p agent-habilis-mesh --no-default-features",
                    // …and must *run* there. Every wasm break this crate has had
                    // compiled cleanly and then panicked: `Instant::now` is
                    // unimplemented on wasm32, `tokio::time` has no driver,
                    // `tokio::spawn` has no reactor. The check above cannot see
                    // any of them — one shipped and killed the browser peer on
                    // load. This suite executes those primitives under node.
                    "test --target wasm32-unknown-unknown -p agent-habilis-mesh --no-default-features --test wasm_runtime",
                ] {
                    let args = args.split(' ');
                    cmd!(sh, "cargo {args...}")
                        .env("CC", &clang)
                        .env("CC_wasm32_unknown_unknown", &clang)
                        .quiet()
                        .run()?;
                }
            }
            None => output::status("Skipping", "wasm32 crate checks (no wasm-capable clang)"),
        }
        // The wasm client is excluded from the workspace, so nothing above reaches it.
        if let Some(clang) = wasm_clang(sh) {
            let _guard = sh.push_dir("crates/agent-share-wasm-client");
            cmd!(sh, "cargo check --target wasm32-unknown-unknown")
                .env("CC", &clang)
                .env("CC_wasm32_unknown_unknown", &clang)
                .quiet()
                .run()?;
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
