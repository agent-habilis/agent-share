//! `cargo task web-image` — build the web app's container image and push it to
//! the self-hosted Gitea registry.
//!
//! Thin on purpose. How the image is built lives in the `Dockerfile` at the repo
//! root, because that is the file Docker reads and a second description of it
//! here would be one more thing to keep true. What this adds is what a
//! Dockerfile cannot state — which registry, which owner, what to call the tag —
//! plus the two guards that otherwise cost minutes: a Docker daemon that is not
//! running, and a dirty tree tagged as though it were a commit.
//!
//! Not part of `ci`: it needs Docker and the network, and it pushes.

use xshell::{Shell, cmd};

use crate::TaskOutcome;
use crate::util::{output, repo_root};

/// The image's name under `<registry>/<owner>/`.
const IMAGE: &str = "agent-share-web";

/// Knobs, mirrored from the `WebImage` variant in `main.rs`.
pub(crate) struct Options {
    pub(crate) tag: Option<String>,
    pub(crate) registry: String,
    pub(crate) owner: String,
    pub(crate) no_push: bool,
    pub(crate) platform: String,
}

pub(crate) fn run(sh: &Shell, opts: &Options) -> TaskOutcome {
    // Docker Desktop stops between sessions, and the error it raises from inside
    // a build reads as a network problem several steps deep.
    if cmd!(sh, "docker info")
        .quiet()
        .ignore_stdout()
        .ignore_stderr()
        .run()
        .is_err()
    {
        return Err("the Docker daemon is not reachable — start Docker and retry".into());
    }

    // The build context is the repo root, not wherever this was invoked: the
    // image builds the wasm from `crates/`, so `web/` is only half of what it
    // needs.
    let _guard = sh.push_dir(repo_root());

    let tag = match &opts.tag {
        Some(tag) => tag.clone(),
        None => default_tag(sh)?,
    };
    let repository = format!("{}/{}/{IMAGE}", opts.registry, opts.owner);
    let pinned = format!("{repository}:{tag}");
    let latest = format!("{repository}:latest");
    let platform = &opts.platform;

    output::status("Building", &format!("{pinned} ({platform})"));
    cmd!(
        sh,
        "docker build --platform {platform} -t {pinned} -t {latest} ."
    )
    .run()?;

    if opts.no_push {
        output::status("Skipping", "push (--no-push)");
        output::status("Finished", &pinned);
        return Ok(());
    }

    // Both references, one push each. `latest` is what the homelab's compose
    // file pulls; the sha tag is what makes a rollback nameable afterwards.
    for reference in [&pinned, &latest] {
        output::status("Pushing", reference);
        cmd!(sh, "docker push {reference}").run()?;
    }

    output::status("Finished", &pinned);
    Ok(())
}

/// The commit this image was built from, marked `-dirty` when the working tree
/// does not match it.
///
/// A sha tag that does not identify a tree is worse than no tag at all: it is
/// the one reached for during a rollback, and it would restore something that
/// was never committed.
fn default_tag(sh: &Shell) -> Result<String, Box<dyn std::error::Error>> {
    let sha = cmd!(sh, "git rev-parse --short HEAD").quiet().read()?;
    let dirty = !cmd!(sh, "git status --porcelain")
        .quiet()
        .read()?
        .trim()
        .is_empty();
    Ok(if dirty { format!("{sha}-dirty") } else { sha })
}
