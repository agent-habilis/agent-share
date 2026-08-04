//! Stage 2a's second kill-gate: this crate must not know what a share is.
//!
//! From RFC 03's isolation rule — `fofoca-blobs` never sees a `MountManifest`, a
//! ticket, a mesh or an ALPN. It takes a key, a size, an mtime and byte ranges.
//! If the trait needs any of those to be useful, the seam is in the wrong place
//! and the crate has quietly become part of `agent-share` rather than a thing
//! `agent-share` uses.
//!
//! A compile-time check would be better than reading the manifest, but a crate
//! cannot ask "am I linked against X" from inside itself. Reading the manifest
//! is the honest approximation, and it fails loudly the moment someone adds the
//! dependency that would make everything easier and the seam meaningless.

use std::path::Path;

/// Dependency names that would mean the seam has collapsed.
const FORBIDDEN: &[&str] = &["agent-share", "agent-share-proto", "agent-habilis-mesh"];

#[test]
fn the_crate_does_not_know_about_agent_share() {
    let manifest =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
            .expect("this crate has a Cargo.toml");

    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        for forbidden in FORBIDDEN {
            assert!(
                !line.starts_with(forbidden),
                "fofoca-blobs must not depend on {forbidden}: found `{line}`.\n\
                 The store takes a key, a size, an mtime and byte ranges. If it \
                 needs a manifest or a mesh to be useful, move the seam rather \
                 than the dependency — see RFC 03, The isolation rule."
            );
        }
    }
}

/// The other half of the rule, in the direction people forget: the crate must
/// stay buildable for the browser, because the browser runs the same store.
///
/// Not a substitute for `cargo check --target wasm32-unknown-unknown`, which CI
/// runs — this only catches a dependency that is *obviously* host-only, before
/// someone waits for a wasm build to tell them.
#[test]
fn no_obviously_host_only_dependency_crept_in() {
    let manifest =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
            .expect("this crate has a Cargo.toml");

    // `tokio` is the one that would slip in most naturally, via someone reaching
    // for `tokio::fs` in a backend. Backends own their I/O; this crate does not.
    for host_only in ["tokio", "nfsserve", "interprocess", "memmap2"] {
        for line in manifest.lines() {
            let line = line.trim();
            if line.starts_with('#') {
                continue;
            }
            assert!(
                !line.starts_with(host_only),
                "fofoca-blobs must build for wasm32; `{host_only}` is host-only.\n\
                 If a backend needs it, the backend is the place for it."
            );
        }
    }
}
