use xshell::{Shell, cmd};

use crate::TaskOutcome;
use crate::util::{ensure_installed, output};

pub(crate) fn run(sh: &Shell) -> TaskOutcome {
    ensure_installed(sh, "cargo-llvm-cov", &["llvm-cov", "--version"]);
    output::status("Running", "tests with coverage");
    cmd!(sh, "cargo llvm-cov --no-report").quiet().run()?;
    output::status("Coverage", "summary");
    cmd!(sh, "cargo llvm-cov report").quiet().run()?;
    Ok(())
}
