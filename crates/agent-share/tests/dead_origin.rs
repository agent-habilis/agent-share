//! The headline claim of "every peer a seeder": a share outlives its
//! producer when someone else on the mesh holds the bytes.
//!
//! RFC 01 phase 3's verification line, as a subprocess test: produce, mirror,
//! **kill the producer**, and prove a fresh consumer — holding the *original*
//! ticket, whose address now points at a corpse — still stands the share up,
//! because the mirror's card vouches for the tree on the mesh and the
//! consumer bootstraps its manifest from it.
//!
//! CI-runnable: the consumer runs `--no-mount`, so the assertion is "the NFS
//! bridge binds and prints the mount command", which cannot happen without a
//! manifest — and with the origin dead, a manifest can only have come from
//! the seeder. The byte path through `SourceSet` needs a real OS mount;
//! exercise it by hand via `mount.rs::real_mount_round_trip`'s recipe with
//! the origin killed mid-read.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::Instant;

mod common;
use common::{CONNECT_TIMEOUT, LOOPBACK_SWARM_ID, POLL, test_cmd};

/// A spawned `agent-share` child killed when the test ends (or panics).
struct ChildGuard(Child);

impl ChildGuard {
    /// The deliberate kill: the whole point of this test.
    fn kill_now(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A throwaway directory under the OS temp dir, removed recursively on drop.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "agent-share-dead-origin-{}-{tag}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn write_file(path: &Path, contents: &[u8]) {
    std::fs::create_dir_all(path.parent().expect("path has a parent")).expect("create parent");
    std::fs::write(path, contents).expect("write file");
}

fn spawn_piped(mut cmd: Command) -> (ChildGuard, Receiver<String>) {
    let mut child = cmd
        .stdout(Stdio::piped())
        .spawn()
        .expect("failed to spawn agent-share process");
    let stdout = child.stdout.take().expect("child stdout handle");
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            match line {
                Ok(text) => {
                    if tx.send(text).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    (ChildGuard(child), rx)
}

fn recv_line_containing(rx: &Receiver<String>, needle: &str) -> Option<String> {
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    while Instant::now() < deadline {
        match rx.recv_timeout(POLL) {
            Ok(line) if line.contains(needle) => return Some(line),
            Ok(_) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return None,
        }
    }
    None
}

fn spawn_producer(root: &Path) -> (ChildGuard, String) {
    let mut cmd = test_cmd();
    cmd.args([
        "serve",
        root.to_str().expect("utf-8 path"),
        "--swarm",
        LOOPBACK_SWARM_ID,
        "--output",
        "json",
    ]);
    let (producer, rx) = spawn_piped(cmd);
    let line =
        recv_line_containing(&rx, "agent-share").expect("producer never printed a mount command");
    let ticket = line
        .split_whitespace()
        .nth(1)
        .expect("mount line missing ticket token")
        .to_string();
    (producer, ticket)
}

#[test]
fn a_share_survives_its_producer_when_a_mirror_serves() {
    let src = TempDir::new("src");
    let root = src.path.join("dataset");
    write_file(&root.join("readme.md"), b"# outlives its producer");
    write_file(&root.join("data/blob.bin"), &vec![9u8; 20_000]);

    let (mut origin, ticket) = spawn_producer(&root);

    // Mirror the whole share — a one-shot copy that leaves the origin's
    // manifest and secret in a sidecar, so `serve` re-serves it as a second
    // source for the *same* share rather than minting a new one.
    let copy = TempDir::new("copy");
    let copy_root = copy.path.join("copy");
    let status = test_cmd()
        .args([
            "mirror",
            &ticket,
            copy_root.to_str().expect("utf-8 path"),
            "--output",
            "json",
        ])
        .status()
        .expect("run mirror");
    assert!(
        status.success(),
        "mirror must complete against a live origin"
    );

    let mut seeder_cmd = test_cmd();
    seeder_cmd.args([
        "serve",
        copy_root.to_str().expect("utf-8 path"),
        "--swarm",
        LOOPBACK_SWARM_ID,
        "--output",
        "json",
    ]);
    let (_seeder, seeder_rx) = spawn_piped(seeder_cmd);
    // The seeder prints its own mount command once it serves; its card (tree +
    // serving) reaches the mesh from there.
    recv_line_containing(&seeder_rx, "agent-share").expect("mirror serve never came up");

    // The whole point.
    origin.kill_now();

    // A fresh consumer holding the ORIGINAL ticket: its address points at the
    // corpse. The short discovery deadline keeps the origin dial from eating
    // the test budget; the manifest must then come from the mirror.
    let target = TempDir::new("mnt");
    let mut consumer_cmd = test_cmd();
    consumer_cmd
        .args([
            &ticket,
            target.path.to_str().expect("utf-8 mount target"),
            "--no-mount",
            "--output",
            "json",
        ])
        .env("AGENT_SHARE_DISCOVERY_DEADLINE_SECS", "5");
    let (_consumer, consumer_rx) = spawn_piped(consumer_cmd);
    let command_line = recv_line_containing(&consumer_rx, "port=")
        .expect("consumer never printed the OS mount command — the seeder bootstrap failed");
    assert!(
        command_line.contains("127.0.0.1:/"),
        "expected an NFS mount command, got: {command_line}"
    );
}
