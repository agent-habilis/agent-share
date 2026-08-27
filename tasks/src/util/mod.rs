use std::path::{Path, PathBuf};

use xshell::{Shell, cmd};

#[expect(
    dead_code,
    reason = "shared cargo-style helpers included from src/cli/output.rs; the task runner uses a subset"
)]
pub(crate) mod output;

/// Workspace root: the parent of this crate's `tasks/` manifest dir.
/// Falls back to CWD if the env var is somehow missing.
pub(crate) fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is tasks/, whose parent is the workspace root.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map_or_else(
            || std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            Path::to_path_buf,
        )
}

/// Install `krate` via `cargo install --locked` if the probe command
/// (`check`) fails. Best-effort: a probe or install hiccup must not
/// abort the calling task — its own command surfaces a clear error if
/// the tool is genuinely missing.
pub(crate) fn ensure_installed(sh: &Shell, krate: &str, check: &[&str]) {
    let ok = cmd!(sh, "cargo {check...}")
        .quiet()
        .ignore_stdout()
        .ignore_stderr()
        .run()
        .is_ok();

    if !ok {
        output::status("Installing", krate);
        let _ = cmd!(sh, "cargo install --locked {krate}").quiet().run();
    }
}

/// Prune `target/` artifacts not touched in the last week. Old build
/// generations from past sessions pile up (feature-flag/profile permutations
/// and `[patch]` source swaps have pushed this tree to tens of GB), while the
/// build that just ran is touched *now* and is always kept. Best-effort:
/// installs `cargo-sweep` on demand and never aborts the calling task.
pub(crate) fn sweep_stale_artifacts(sh: &Shell) {
    ensure_installed(sh, "cargo-sweep", &["sweep", "--version"]);
    output::status("Pruning", "build artifacts older than 7 days");
    let _ = cmd!(sh, "cargo sweep --time 7").quiet().run();
}

/// A cargo profile `build_binary` may build under. Cargo writes a profile's
/// output to `target/<profile>/`, so the name is the only input the path
/// needs. `dev` (which writes to `target/debug`) is deliberately absent: a
/// debug build would measure the optimizer rather than the protocol, so both
/// variants optimize — and *which* one a caller takes is deliberate.
#[derive(Clone, Copy)]
pub(crate) enum Profile {
    /// Thin LTO over 16 codegen units (see `[profile.ci]` in the root
    /// manifest). For `e2e`, which asserts pass/fail and only pays for the
    /// link.
    Ci,
    /// Full release. For `bench`: a throughput number is comparable only to
    /// others measured under the same inlining.
    Release,
}

impl Profile {
    fn name(self) -> &'static str {
        match self {
            Self::Ci => "ci",
            Self::Release => "release",
        }
    }
}

/// Build the `agent-share` binary under `profile` and return its path.
pub(crate) fn build_binary(
    sh: &Shell,
    profile: Profile,
) -> Result<String, Box<dyn std::error::Error>> {
    let profile = profile.name();
    output::status("Building", &format!("agent-share ({profile})"));
    cmd!(sh, "cargo build --profile {profile} -p agent-share")
        .quiet()
        .run()?;
    let binary = repo_root().join("target").join(profile).join("agent-share");
    if !binary.exists() {
        return Err(format!("{profile} binary missing at {}", binary.display()).into());
    }
    Ok(binary.display().to_string())
}
