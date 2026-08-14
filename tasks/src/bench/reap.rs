//! Crash-safe cleanup for everything a bench run spawns.
//!
//! [`super::proc::Proc`] kills and reaps on `Drop`, which covers a normal exit,
//! an early `?` and a panic. It does **not** cover a signal: `SIGINT` from a
//! user's Ctrl-C terminates the runner outright, destructors never run, and a
//! bench producer, a bun dev server, a headless Chrome and a live NFS mount are
//! all left behind. Handling the signal would need a handler crate or an
//! `unsafe` `libc::signal`, and would still miss `SIGKILL`.
//!
//! So instead of trying to always run cleanup on the way *out*, the run records
//! what it owns on the way *in*, and the next run clears anything the last one
//! left. That covers Ctrl-C, `SIGKILL`, a panic, and a power cut alike.
//!
//! Recorded pids are re-checked against the command line we expected before
//! anything is killed — a pid is recycled quickly, and this must never kill a
//! `serve` the user started themselves.

use std::path::PathBuf;
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::util::{self, output};

/// Lives under `target/`, which is already ignored and already disposable.
const STATE_FILE: &str = "target/bench-state.json";

#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct State {
    /// `(pid, marker)` — `marker` must still appear in the process's command
    /// line for it to be considered ours.
    pids: Vec<(u32, String)>,
    /// Mountpoints to `umount` if they are still mounted.
    mounts: Vec<String>,
    /// Folder keys headless windows were launched against. A set rather than
    /// one slot: a cell that stands up two peers has two windows, and tracking
    /// the second used to forget the first — which leaked it on a hard kill.
    #[serde(default)]
    browser_folders: Vec<String>,
}

fn path() -> PathBuf {
    util::repo_root().join(STATE_FILE)
}

fn load() -> State {
    std::fs::read_to_string(path())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn store(state: &State) {
    let file = path();
    if let Some(parent) = file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string(state) {
        let _ = std::fs::write(file, text);
    }
}

/// Record a process this run owns.
pub(crate) fn track_pid(pid: u32, marker: &str) {
    let mut state = load();
    state.pids.push((pid, marker.to_owned()));
    store(&state);
}

/// Forget a process that has already been reaped.
pub(crate) fn untrack_pid(pid: u32) {
    let mut state = load();
    state.pids.retain(|(known, _)| *known != pid);
    store(&state);
}

/// Record a mountpoint that must be unmounted even if we die first.
pub(crate) fn track_mount(mountpoint: &str) {
    let mut state = load();
    state.mounts.push(mountpoint.to_owned());
    store(&state);
}

pub(crate) fn untrack_mount(mountpoint: &str) {
    let mut state = load();
    state.mounts.retain(|known| known != mountpoint);
    store(&state);
}

pub(crate) fn track_browser(folder: &str) {
    let mut state = load();
    if !state.browser_folders.iter().any(|known| known == folder) {
        state.browser_folders.push(folder.to_owned());
    }
    store(&state);
}

/// Forget one window. Takes the folder, so a guard dropping its own window
/// cannot untrack a sibling that is still open.
pub(crate) fn untrack_browser(folder: &str) {
    let mut state = load();
    state.browser_folders.retain(|known| known != folder);
    store(&state);
}

/// Clear anything a previous run left behind. Safe to call when there is none.
pub(crate) fn reap_stale() {
    let state = load();
    if state.pids.is_empty() && state.mounts.is_empty() && state.browser_folders.is_empty() {
        return;
    }
    output::status_warn("Reaping", "leftovers from an interrupted bench run");

    for mountpoint in &state.mounts {
        if is_mounted(mountpoint) {
            output::warn(&format!("unmounting stale mount {mountpoint}"));
            let _ = Command::new("umount").arg(mountpoint).output();
        }
    }
    for (pid, marker) in &state.pids {
        if owns(*pid, marker) {
            output::warn(&format!("killing stale process {pid} ({marker})"));
            let _ = Command::new("/bin/kill")
                .arg("-INT")
                .arg(pid.to_string())
                .output();
            std::thread::sleep(std::time::Duration::from_millis(500));
            let _ = Command::new("/bin/kill")
                .args(["-9", &pid.to_string()])
                .output();
        }
    }
    for folder in &state.browser_folders {
        let _ = Command::new("agent-browse").args(["quit", folder]).output();
    }
    store(&State::default());
}

/// Is `pid` alive *and* still the process we spawned?
///
/// The marker check is the load-bearing half: pids are recycled, and killing a
/// stranger — or the user's own `agent-share serve` — would be far worse than
/// leaking one process.
fn owns(pid: u32, marker: &str) -> bool {
    Command::new("ps")
        .args(["-o", "command=", "-p", &pid.to_string()])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .is_some_and(|output| String::from_utf8_lossy(&output.stdout).contains(marker))
}

fn is_mounted(mountpoint: &str) -> bool {
    Command::new("mount")
        .output()
        .ok()
        .is_some_and(|output| String::from_utf8_lossy(&output.stdout).contains(mountpoint))
}
