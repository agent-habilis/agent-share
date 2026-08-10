//! The kill-gate on the seam: this crate must build for both targets.
//!
//! It exists because the mount protocol server was written twice, once for the
//! CLI and once for the browser, and two copies of a protocol do not stay
//! equal. The only thing keeping them from splitting again is that this crate
//! *cannot* take a dependency that works on one side and not the other — the
//! moment it can, the easy fix for any awkwardness is a `cfg` that quietly
//! forks the behaviour, and the copies are back.
//!
//! `fofoca-chunks/tests/isolation.rs` guards a different rule with the same
//! trick: it forbids the network layer. This one forbids *platforms*.
//!
//! A compile-time check would be better than reading the manifest, but a crate
//! cannot ask "am I linked against X" from inside itself. Reading the manifest
//! is the honest approximation, and it fails loudly the moment somebody adds
//! the dependency that would make one platform's problem go away.

use std::path::Path;

/// Dependency-name prefixes that exist on one target and not the other.
///
/// `tokio` and `mio` are the host's; the browser has no reactor and no
/// sockets. `wasm-bindgen`, `web-sys` and `js-sys` are the browser's; a host
/// build never sees them. `nfsserve` and `notify` are the CLI's alone.
///
/// Prefixes rather than names, so `tokio-util` and `wasm-bindgen-futures` are
/// caught too — a platform arrives through its family, not through one crate.
const PLATFORM_ONLY: &[&str] = &[
    "tokio",
    "mio",
    "wasm-bindgen",
    "web-sys",
    "js-sys",
    "nfsserve",
    "notify",
];

/// Forbidden by *name*, for a different reason: this crate sits below both
/// callers, so an edge back into either is a layering inversion as well as a
/// way to drag a whole platform in behind it.
///
/// Exact names, not prefixes — `agent-share-proto` is the shared wire crate and
/// is exactly what this one is supposed to build on.
const CALLERS: &[&str] = &["agent-share", "agent-share-wasm-client"];

/// Lines that name the package rather than a dependency. Everything else in a
/// `[dependencies]`-shaped line begins with the crate being depended on.
fn is_metadata(line: &str) -> bool {
    line.starts_with('#')
        || line.starts_with('[')
        || line.starts_with("name")
        || line.starts_with("repository")
        || line.starts_with("description")
}

/// The crate a dependency line names: everything up to the `=` of
/// `dep = { … }` or the `.` of `dep.workspace = true`.
fn dep_name(line: &str) -> &str {
    line.split([' ', '=', '.']).next().unwrap_or("")
}

fn manifest() -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
        .expect("this crate has a Cargo.toml")
}

#[test]
fn the_crate_belongs_to_neither_platform() {
    for line in manifest().lines() {
        let line = line.trim();
        if is_metadata(line) {
            continue;
        }
        let dep = dep_name(line);
        if dep.is_empty() {
            continue;
        }
        assert!(
            !PLATFORM_ONLY.iter().any(|family| dep.starts_with(family)),
            "`agent-share-mount` must build for the host and for wasm32, so it \
             cannot depend on `{dep}`. Whatever needed it belongs in the caller: \
             `agent-share` for the CLI, `agent-share-wasm-client` for the \
             browser.\n  offending line: {line}"
        );
        assert!(
            !CALLERS.contains(&dep),
            "`agent-share-mount` sits below `{dep}`, so depending on it inverts \
             the layering. Move the shared part down here instead.\n  offending \
             line: {line}"
        );
    }
}

/// The crate is useless if it cannot name the types it dispatches over, and a
/// manifest that parses proves nothing about that. This is the smallest thing
/// that fails on a target where the streams are unavailable.
#[test]
fn the_dispatch_entry_point_is_reachable() {
    let _ = agent_share_mount::serve_stream::<Never>;
}

/// A source that cannot exist, only named. Implementing the trait at all is
/// the check: it proves every associated future and the watcher seam compile
/// on this target, which is the half a manifest cannot tell you about.
#[derive(Clone)]
enum Never {}

impl agent_share_mount::Watcher for Never {
    type Frame = Box<Vec<u8>>;
    async fn recv(&mut self) -> Option<Self::Frame> {
        match *self {}
    }
}

impl agent_share_mount::ServeSource for Never {
    type Watcher = Self;

    fn manifest_envelope(&self) -> Option<Vec<u8>> {
        match *self {}
    }
    fn subscribe(&self) -> Option<(Vec<u8>, Self::Watcher)> {
        match *self {}
    }
    async fn answer_read(
        &self,
        _index: u32,
        _offset: u64,
        _len: u32,
    ) -> (agent_share_proto::manifest::ReadStatus, Vec<u8>) {
        match *self {}
    }
    async fn answer_chunk_map(&self, _index: u32) -> Option<fofoca_chunks::ChunkMap> {
        match *self {}
    }
    async fn answer_chunk(&self, _address: fofoca_chunks::ChunkHash) -> Option<Vec<u8>> {
        match *self {}
    }
    async fn answer_have(&self, _root: fofoca_chunks::Root) -> Option<fofoca_chunks::Coverage> {
        match *self {}
    }
}

/// A `[target.'cfg(...)']` block would be the natural way to smuggle a
/// platform-only dependency past the list above, so it is refused outright.
/// `fofoca-chunks` legitimately has them — it has a browser storage backend —
/// but this crate has no per-platform half, and the day it grows one is the day
/// the copies start drifting again.
#[test]
fn nothing_is_conditional_on_the_target() {
    for line in manifest().lines() {
        assert!(
            !line.trim_start().starts_with("[target."),
            "`agent-share-mount` has no per-platform half, and a target block is \
             how it would get one.\n  offending line: {line}"
        );
    }
}
