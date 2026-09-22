//! File and directory naming: snake_case inside a crate, kebab-case everywhere
//! else.
//!
//! Rust needs its own separator because a module file name *is* the module
//! name — `mod mesh_key;` only ever finds `mesh_key.rs`, and spelling it
//! `mesh-key.rs` costs a `#[path]` attribute on every declaration.
//!
//! Only tracked files are checked, so generated trees the rule cannot reach
//! (`packages/agent-share-wasm/src/glue/`, `target/`, `dist/`) are out of
//! scope by construction rather than by an exclusion list.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use xshell::{Shell, cmd};

use crate::TaskOutcome;
use crate::util::repo_root;

/// Names the ecosystem picks for us, allowed in both zones.
///
/// `Cargo.toml`/`Cargo.lock` are cargo's own spelling. `README.md` is declared
/// as `readme = "README.md"` by three manifests, so it is load-bearing rather
/// than conventional. `Dockerfile` is what `cargo task web-image` builds with,
/// passing no `-f`. `LICENSE` is convention only — every manifest uses the
/// SPDX `license` field — but renaming it would lose crates.io and GitHub
/// detection for nothing. `CLAUDE.md`/`AGENTS.md` are here before they exist:
/// `clippy.toml` already points at a `CLAUDE.md`, and the gate should not be
/// what stops someone committing one. `Formula` is the folder Homebrew reads
/// in a tap, and the release workflow copies ours into the tap as-is.
const ALLOWED: &[&str] = &[
    "AGENTS.md",
    "CLAUDE.md",
    "Cargo.lock",
    "Cargo.toml",
    "Dockerfile",
    "Formula",
    "LICENSE",
    "README.md",
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Zone {
    Kebab,
    Snake,
}

impl Zone {
    const fn separator(self) -> char {
        match self {
            Self::Kebab => '-',
            Self::Snake => '_',
        }
    }

    const fn describe(self) -> &'static str {
        match self {
            Self::Kebab => "kebab-case (lowercase and dashes)",
            Self::Snake => "snake_case (lowercase and underscores, inside a crate)",
        }
    }
}

pub(crate) fn run(sh: &Shell) -> TaskOutcome {
    // `cargo task` runs from wherever it was invoked, and `git ls-files` from a
    // subdirectory lists only that subtree.
    let _guard = sh.push_dir(repo_root());

    let files = tracked_files(sh)?;
    let roots = crate_roots(&files, sh);

    // Keyed by path so a bad directory is reported once rather than once per
    // file beneath it; `BTreeMap` also sorts the report for free.
    let mut offenders = BTreeMap::new();
    for file in &files {
        let components: Vec<_> = file.split('/').collect();
        for end in 1..=components.len() {
            let prefix = components[..end].join("/");
            let zone = zone_of(&components[..end], &roots);
            if !is_valid(components[end - 1], zone) {
                offenders.insert(prefix, (components[end - 1], zone));
            }
        }
    }

    if offenders.is_empty() {
        return Ok(());
    }

    let mut report = format!("{} paths break the naming rule:\n", offenders.len());
    for (path, (name, zone)) in &offenders {
        let _ = write!(
            report,
            "\n  {path}\n    want {} — suggested: {}",
            zone.describe(),
            suggest(name, *zone),
        );
    }
    Err(report.into())
}

/// Tracked files only. `--others` would pull in `.DS_Store`, which this repo's
/// `.gitignore` does not cover — it is excluded by a *global* ignore file on
/// some machines and nothing on others, so the gate's verdict would depend on
/// whose checkout it ran in.
///
/// `-z` because `git ls-files` shell-quotes any path it considers unusual,
/// which would then fail the check on the quotes it added itself.
fn tracked_files(sh: &Shell) -> Result<Vec<String>, xshell::Error> {
    let listing = cmd!(sh, "git ls-files -z").quiet().read()?;
    Ok(listing
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect())
}

/// Directories holding a `Cargo.toml` with a `[package]` section.
///
/// The `[package]` part is the whole point: the repo root's manifest is a
/// *virtual* workspace one, so keying on the file alone would put every path in
/// the repo inside a crate and demand snake_case of `packages/` and
/// `tsconfig.base.json`. Scanning for the section header rather than parsing
/// the TOML is a heuristic, but the only thing it has to tell apart is a
/// virtual manifest from a real one.
///
/// Derived rather than hardcoded so a new crate is covered the day it lands.
fn crate_roots(files: &[String], sh: &Shell) -> BTreeSet<String> {
    files
        .iter()
        .filter(|file| file.ends_with("Cargo.toml"))
        .filter(|file| {
            sh.read_file(file)
                .is_ok_and(|text| text.lines().any(|line| line.trim() == "[package]"))
        })
        .map(|file| {
            file.trim_end_matches("Cargo.toml")
                .trim_end_matches('/')
                .to_owned()
        })
        .collect()
}

/// The zone the last component of `prefix` belongs to.
///
/// A crate root's own name is a package name, so it stays kebab
/// (`crates/agent-share-proto/`); only what lives *under* it is Rust module
/// naming. Ancestors of a crate root stay kebab for the same reason, which is
/// what keeps a future `crates/agent-share/fuzz/` sub-crate working.
fn zone_of(prefix: &[&str], roots: &BTreeSet<String>) -> Zone {
    let path = prefix.join("/");
    if roots.contains(&path)
        || roots
            .iter()
            .any(|root| root.starts_with(&format!("{path}/")))
    {
        return Zone::Kebab;
    }
    let inside_crate = roots
        .iter()
        .any(|root| !root.is_empty() && path.starts_with(&format!("{root}/")));
    if inside_crate {
        Zone::Snake
    } else {
        Zone::Kebab
    }
}

/// Leading dots are stripped once so `.gitignore` and `.cargo` pass while
/// `..odd` still fails. Doubled and trailing separators are deliberately not
/// rejected: more surface, and nothing in the tree does it.
///
/// Rejecting every uppercase letter is what catches PascalCase and camelCase;
/// rejecting the *other* zone's separator is what keeps the two conventions
/// from bleeding into each other.
fn is_valid(name: &str, zone: Zone) -> bool {
    if ALLOWED.contains(&name) {
        return true;
    }
    let name = name.strip_prefix('.').unwrap_or(name);
    let Some(first) = name.chars().next() else {
        return false;
    };
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return false;
    }
    name.chars().all(|char| {
        char.is_ascii_lowercase()
            || char.is_ascii_digit()
            || char == '.'
            || char == zone.separator()
    })
}

/// A copy-pasteable replacement, so a failing gate reads as a rename list
/// rather than a complaint. Splits on camel/Pascal boundaries and on the wrong
/// separator, keeping the extension intact: `SlotGrid` becomes `slot-grid`,
/// `Badge.test.tsx` becomes `badge.test.tsx`.
fn suggest(name: &str, zone: Zone) -> String {
    let (stem, extension) = name
        .split_once('.')
        .map_or((name, ""), |(stem, rest)| (stem, rest));
    let mut out = String::new();
    for (index, char) in stem.char_indices() {
        if char == '-' || char == '_' {
            out.push(zone.separator());
            continue;
        }
        // A boundary is a capital that follows a lowercase or digit, so
        // `SlotGrid` splits once and an acronym like `HTTPServer` splits only
        // before `Server`.
        let previous = stem[..index].chars().next_back();
        let starts_word = char.is_ascii_uppercase()
            && previous.is_some_and(|previous| {
                previous.is_ascii_lowercase()
                    || previous.is_ascii_digit()
                    || stem[index + char.len_utf8()..]
                        .chars()
                        .next()
                        .is_some_and(|next| next.is_ascii_lowercase())
            });
        if starts_word && !out.is_empty() {
            out.push(zone.separator());
        }
        out.extend(char.to_lowercase());
    }
    if extension.is_empty() {
        out
    } else {
        format!("{out}.{extension}")
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{Zone, is_valid, suggest, zone_of};

    fn roots() -> BTreeSet<String> {
        ["crates/agent-share", "tasks"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    }

    fn zone(path: &str) -> Zone {
        let components: Vec<_> = path.split('/').collect();
        zone_of(&components, &roots())
    }

    #[test]
    fn crate_roots_and_their_ancestors_stay_kebab() {
        assert!(zone("crates") == Zone::Kebab);
        assert!(zone("crates/agent-share") == Zone::Kebab);
        assert!(zone("tasks") == Zone::Kebab);
    }

    #[test]
    fn paths_under_a_crate_root_are_snake() {
        assert!(zone("crates/agent-share/src") == Zone::Snake);
        assert!(zone("crates/agent-share/src/mesh_key.rs") == Zone::Snake);
        assert!(zone("tasks/src/web_image.rs") == Zone::Snake);
    }

    #[test]
    fn everything_outside_a_crate_is_kebab() {
        assert!(zone("packages/agent-share-web/src/lib/peer-card") == Zone::Kebab);
        assert!(zone("scripts/build-ip-country.ts") == Zone::Kebab);
    }

    #[test]
    fn each_zone_rejects_the_other_separator() {
        assert!(is_valid("mesh_key.rs", Zone::Snake));
        assert!(!is_valid("mesh-key.rs", Zone::Snake));
        assert!(is_valid("peer-card", Zone::Kebab));
        assert!(!is_valid("peer_card", Zone::Kebab));
    }

    #[test]
    fn uppercase_is_rejected_in_both_zones() {
        assert!(!is_valid("TechInfo", Zone::Kebab));
        assert!(!is_valid("peerCard", Zone::Kebab));
        assert!(!is_valid("MeshKey.rs", Zone::Snake));
    }

    #[test]
    fn dotfiles_and_ecosystem_names_pass() {
        assert!(is_valid(".gitignore", Zone::Kebab));
        assert!(is_valid(".cargo", Zone::Kebab));
        assert!(is_valid("Cargo.toml", Zone::Snake));
        assert!(is_valid("README.md", Zone::Snake));
        assert!(is_valid("Formula", Zone::Kebab));
        assert!(!is_valid("..odd", Zone::Kebab));
    }

    #[test]
    fn suggestions_are_copy_pasteable() {
        assert_eq!(suggest("SlotGrid", Zone::Kebab), "slot-grid");
        assert_eq!(suggest("peerCard", Zone::Kebab), "peer-card");
        assert_eq!(suggest("Badge.test.tsx", Zone::Kebab), "badge.test.tsx");
        assert_eq!(suggest("MeshKey.rs", Zone::Snake), "mesh_key.rs");
        assert_eq!(suggest("HTTPServer", Zone::Kebab), "http-server");
    }
}
