//! What `serve` prints for a person to copy, and that the printed command runs.
//!
//! The `Mount` line is pasted as it is, so it must not carry a target the
//! consumer does not need. The `Open` line is the same share in the webapp.

use std::io::{BufRead, BufReader};
use std::process::{Child, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::Instant;

use self::common::{CONNECT_TIMEOUT, LOOPBACK_SWARM_ID, POLL, test_cmd};

mod common;

/// A spawned `agent-share` child killed when the test ends (or panics).
struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
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

#[test]
fn serve_prints_a_bare_mount_command_and_a_web_link() {
    let dir = tempfile::Builder::new()
        .prefix("agent-share-serve-output-")
        .tempdir()
        .expect("temp dir");
    std::fs::write(dir.path().join("readme.md"), b"# hi").expect("write file");

    let mut child = test_cmd()
        .args([
            "serve",
            dir.path().to_str().expect("utf-8 path"),
            "--swarm",
            LOOPBACK_SWARM_ID,
        ])
        .stdout(Stdio::piped())
        .spawn()
        .expect("failed to spawn agent-share serve");
    let stdout = child.stdout.take().expect("child stdout handle");
    let _producer = ChildGuard(child);
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    let mount = recv_line_containing(&rx, "Mount").expect("serve never printed a Mount line");
    let command: Vec<&str> = mount.split_whitespace().skip(1).collect();
    assert_eq!(
        command.len(),
        2,
        "the Mount line must be exactly `agent-share <ticket>`, got: {mount}"
    );
    let ticket = command[1];

    let open = recv_line_containing(&rx, "Open").expect("serve never printed an Open line");
    assert!(
        open.ends_with(&format!("https://agent-share.dev/app/files/{ticket}")),
        "the Open line must link the share in the webapp, got: {open}"
    );
}

#[test]
fn the_consumer_form_needs_no_target() {
    let output = test_cmd()
        .arg("not-a-ticket")
        .output()
        .expect("run agent-share");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "a bad ticket must still fail");
    assert!(
        !stderr.contains("usage:"),
        "a missing target must default to the current folder, not be a usage error: {stderr}"
    );
}
