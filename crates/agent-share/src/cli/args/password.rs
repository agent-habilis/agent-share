//! The `--password` / `--password-stdin` pair, flattened into every command
//! that either mints or redeems a ticket.
//!
//! Two ways in and no prompt, deliberately: this binary is driven from scripts
//! as often as from a terminal, and a TTY prompt is the one form neither a pipe
//! nor a CI job can satisfy. `--password-stdin` is the form to reach for; the
//! literal exists because typing a password once at a shell is the common case
//! and refusing it would only push people to `echo`, which leaks the same way
//! for more effort.

use std::io::Read as _;

use anyhow::{Context, Result, bail};
use clap::Parser;

/// How a password reaches a share, on either side of it.
#[derive(Parser, Debug)]
pub(crate) struct PasswordArgs {
    /// Password for the share. On `serve` this protects it; on a consumer it
    /// unlocks one.
    ///
    /// Note this lands in `ps` output and your shell history — prefer
    /// `--password-stdin` anywhere that matters.
    #[arg(long, value_name = "PASSWORD", conflicts_with = "password_stdin")]
    pub password: Option<String>,

    /// Read the password from stdin instead of the command line, taking
    /// everything up to the first newline. The form for scripts and CI.
    #[arg(long)]
    pub password_stdin: bool,
}

impl PasswordArgs {
    /// The password, or `None` when neither flag was given.
    ///
    /// # Errors
    /// `--password-stdin` was passed and stdin could not be read, or what it
    /// carried was empty. An empty password is refused rather than silently
    /// treated as "no password": the two mean opposite things, and a pipeline
    /// that ran dry must not quietly publish an unprotected share.
    pub(crate) fn resolve(&self) -> Result<Option<String>> {
        if let Some(password) = &self.password {
            if password.is_empty() {
                bail!("--password was empty; omit the flag to leave the share unprotected");
            }
            return Ok(Some(password.clone()));
        }
        if !self.password_stdin {
            return Ok(None);
        }
        let mut raw = String::new();
        std::io::stdin()
            .read_to_string(&mut raw)
            .context("reading the password from stdin")?;
        // Up to the first newline, so `echo pw | …` works without the trailing
        // byte becoming part of the credential. Nothing else is trimmed: a
        // password may legitimately start or end with a space.
        let password = raw.split('\n').next().unwrap_or_default();
        let password = password.strip_suffix('\r').unwrap_or(password);
        if password.is_empty() {
            bail!("--password-stdin read an empty password");
        }
        Ok(Some(password.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::cli::args::{Cli, MountAction};

    fn serve_password(args: &[&str]) -> Option<String> {
        let Some(MountAction::Serve { password, .. }) = Cli::parse_from(args).action else {
            panic!("expected Serve");
        };
        password.resolve().expect("resolve")
    }

    #[test]
    fn absent_is_no_password() {
        assert_eq!(serve_password(&["agent-share", "serve", "./dir"]), None);
    }

    #[test]
    fn the_literal_flag_is_taken_verbatim() {
        assert_eq!(
            serve_password(&["agent-share", "serve", "./dir", "--password", " hunter2 "]),
            Some(" hunter2 ".to_owned()),
            "a password may legitimately be padded; only the newline is ours to strip"
        );
    }

    #[test]
    fn an_empty_literal_is_refused() {
        let Some(MountAction::Serve { password, .. }) =
            Cli::parse_from(["agent-share", "serve", "./dir", "--password", ""]).action
        else {
            panic!("expected Serve");
        };
        assert!(
            password.resolve().is_err(),
            "an empty password must not read as an unprotected share"
        );
    }

    #[test]
    fn the_two_forms_conflict() {
        assert!(
            Cli::try_parse_from([
                "agent-share",
                "serve",
                "./dir",
                "--password",
                "pw",
                "--password-stdin",
            ])
            .is_err(),
            "naming both leaves it ambiguous which one wins"
        );
    }

    #[test]
    fn the_consumer_form_takes_a_password_too() {
        let cli = Cli::parse_from(["agent-share", "abc", "./mnt", "--password", "pw"]);
        assert_eq!(
            cli.password.resolve().expect("resolve").as_deref(),
            Some("pw")
        );
    }

    #[test]
    fn seed_takes_a_password_too() {
        let Some(MountAction::Seed { password, .. }) =
            Cli::parse_from(["agent-share", "seed", "abc", "./dest", "--password", "pw"]).action
        else {
            panic!("expected Seed");
        };
        assert_eq!(password.resolve().expect("resolve").as_deref(), Some("pw"));
    }

    #[test]
    fn seed_serves_unless_told_to_copy_only() {
        let parse = |args: &[&str]| match Cli::parse_from(args).action {
            Some(MountAction::Seed { copy_only, .. }) => copy_only,
            _ => panic!("expected Seed"),
        };
        assert!(!parse(&["agent-share", "seed", "abc", "./dest"]));
        assert!(parse(&[
            "agent-share",
            "seed",
            "abc",
            "./dest",
            "--copy-only"
        ]));
    }
}
