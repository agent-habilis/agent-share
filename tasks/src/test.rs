use xshell::{Shell, cmd};

use crate::TaskOutcome;

pub(crate) fn run(sh: &Shell) -> TaskOutcome {
    cmd!(sh, "cargo test").quiet().run()?;

    crate::util::sweep_stale_artifacts(sh);
    Ok(())
}
