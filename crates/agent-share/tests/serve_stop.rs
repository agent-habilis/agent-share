//! What `serve` does with a Ctrl-C: say so at once, then shut down cleanly.
//!
//! A clean shutdown takes seconds (the goodbye to the mesh, then the endpoint
//! closes), so the `Stopping` line is the only sign the Ctrl-C was heard. A
//! Ctrl-C during startup must get the same treatment: before the fix it took
//! the default action and killed the process with no goodbye at all.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use self::common::{CONNECT_TIMEOUT, LOOPBACK_SWARM_ID, POLL, test_cmd};

mod common;

/// The `Stopping` line has to come at once, long before the shutdown ends.
const STOPPING_TIMEOUT: Duration = Duration::from_secs(1);

/// Covers the ~7 s clean shutdown with room to spare.
const EXIT_TIMEOUT: Duration = Duration::from_secs(20);

/// A spawned `agent-share` child killed when the test ends (or panics).
struct ChildGuard(Child);

impl ChildGuard {
    /// Ctrl-C, not a kill: the graceful path is the one under test.
    fn interrupt(&self) {
        let status = Command::new("kill")
            .args(["-INT", &self.0.id().to_string()])
            .status()
            .expect("run kill");
        assert!(status.success(), "kill -INT failed");
    }

    fn exit_status_within(&mut self, within: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            if let Ok(Some(status)) = self.0.try_wait() {
                return Some(status);
            }
            thread::sleep(POLL);
        }
        None
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Both streams into one channel: `Mount` is on stdout, `Stopping` on stderr.
fn spawn_serve() -> (ChildGuard, Receiver<String>, tempfile::TempDir) {
    let dir = tempfile::Builder::new()
        .prefix("agent-share-serve-stop-")
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
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn agent-share serve");
    let (tx, rx) = mpsc::channel();
    forward_lines(
        child.stdout.take().expect("child stdout handle"),
        tx.clone(),
    );
    forward_lines(child.stderr.take().expect("child stderr handle"), tx);
    (ChildGuard(child), rx, dir)
}

fn forward_lines(stream: impl std::io::Read + Send + 'static, tx: mpsc::Sender<String>) {
    thread::spawn(move || {
        for line in BufReader::new(stream).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
}

fn recv_line_within(rx: &Receiver<String>, needle: &str, within: Duration) -> Option<String> {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        match rx.recv_timeout(POLL) {
            Ok(line) if line.contains(needle) => return Some(line),
            Ok(_) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return None,
        }
    }
    None
}

/// Ctrl-C after `after_mount`, then the checks every case shares.
fn assert_stops_cleanly(after_mount: Duration) {
    let (mut serve, rx, _dir) = spawn_serve();
    recv_line_within(&rx, "Mount", CONNECT_TIMEOUT).expect("serve never printed a Mount line");
    thread::sleep(after_mount);

    serve.interrupt();

    assert!(
        recv_line_within(&rx, "Stopping", STOPPING_TIMEOUT).is_some(),
        "no Stopping line within {STOPPING_TIMEOUT:?} of the Ctrl-C"
    );
    let status = serve
        .exit_status_within(EXIT_TIMEOUT)
        .expect("serve did not exit after its Ctrl-C");
    assert!(
        status.success(),
        "serve must shut down cleanly after a Ctrl-C, got {status}"
    );
}

#[test]
fn a_ctrl_c_right_after_start_stops_serve_cleanly() {
    assert_stops_cleanly(Duration::ZERO);
}

#[test]
fn a_ctrl_c_while_serving_prints_stopping_at_once() {
    assert_stops_cleanly(Duration::from_secs(5));
}
