//! A consumer that stops on Ctrl-C says goodbye on the mesh.
//!
//! Without the goodbye the producer keeps counting the consumer as present
//! until a silence timeout expires, so its `Peers` line lies for that long. The
//! assertion is on that line: it has to drop back to zero well inside the
//! timeout, which only a `Left` announcement can make happen.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use self::common::{CONNECT_TIMEOUT, LOOPBACK_SWARM_ID, POLL, test_cmd};

mod common;

/// Far less than the mesh's silence timeout, and far more than a `Left` needs
/// to cross loopback.
const GOODBYE_TIMEOUT: Duration = Duration::from_secs(10);

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
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Both streams into one channel: the mount command is on stdout, the `Peers`
/// line on stderr.
fn spawn_piped(mut cmd: Command) -> (ChildGuard, Receiver<String>) {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn agent-share process");
    let (tx, rx) = mpsc::channel();
    forward_lines(
        child.stdout.take().expect("child stdout handle"),
        tx.clone(),
    );
    forward_lines(child.stderr.take().expect("child stderr handle"), tx);
    (ChildGuard(child), rx)
}

fn forward_lines(stream: impl std::io::Read + Send + 'static, tx: mpsc::Sender<String>) {
    thread::spawn(move || {
        for line in BufReader::new(stream).lines() {
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

/// Human output on purpose: the `Peers` line is only printed there.
fn spawn_producer(root: &Path) -> (ChildGuard, Receiver<String>, String) {
    let mut cmd = test_cmd();
    cmd.args([
        "serve",
        root.to_str().expect("utf-8 path"),
        "--swarm",
        LOOPBACK_SWARM_ID,
    ]);
    let (producer, rx) = spawn_piped(cmd);
    let line = recv_line_within(&rx, "Mount", CONNECT_TIMEOUT)
        .expect("producer never printed a mount command");
    let ticket = line
        .split_whitespace()
        .skip_while(|word| *word != "agent-share")
        .nth(1)
        .expect("mount line missing ticket token")
        .to_string();
    (producer, rx, ticket)
}

#[test]
fn a_consumer_leaves_the_mesh_on_ctrl_c() {
    let dir = tempfile::Builder::new()
        .prefix("agent-share-leave-")
        .tempdir()
        .expect("temp dir");
    let root = dir.path().join("share");
    std::fs::create_dir_all(&root).expect("create share dir");
    std::fs::write(root.join("readme.md"), b"# goodbye").expect("write file");

    let (_producer, producer_rx, ticket) = spawn_producer(&root);

    let target = dir.path().join("mnt");
    let mut consumer_cmd = test_cmd();
    consumer_cmd.args([
        &ticket,
        target.to_str().expect("utf-8 mount target"),
        "--no-mount",
        "--output",
        "json",
    ]);
    let (consumer, consumer_rx) = spawn_piped(consumer_cmd);
    recv_line_within(&consumer_rx, "port=", CONNECT_TIMEOUT)
        .expect("consumer never printed the OS mount command");
    recv_line_within(&producer_rx, "1 on mesh", CONNECT_TIMEOUT)
        .expect("the producer never saw the consumer join the mesh");

    consumer.interrupt();

    assert!(
        recv_line_within(&producer_rx, "0 on mesh", GOODBYE_TIMEOUT).is_some(),
        "the producer still counted the consumer {GOODBYE_TIMEOUT:?} after its Ctrl-C: no Left was sent"
    );
}
