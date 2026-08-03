//! Subprocess plumbing for `cargo task bench`.
//!
//! Ported from the shape `crates/agent-share/tests/mount.rs` already proved out
//! (`ChildGuard` / `spawn_piped` / `recv_line_containing`), which cannot be
//! imported here: it is test-local to another crate. The pieces the harness
//! needs beyond that test are a graceful `SIGINT` (the mount consumer only
//! unmounts on Ctrl-C, `mount/consume.rs:150`) and a run-to-completion capture
//! for the consumers that exit on their own.
//!
//! Every spawned process is owned by a [`Proc`], which kills and reaps on drop
//! — including on panic and on `?`-propagation, so a failed cell cannot leave a
//! producer holding a port or a mount holding a filesystem.

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Task-local result alias; matches the runner's boxed-error convention.
pub(crate) type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// How long to wait for an interrupted child to exit before killing it.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

/// Poll interval while waiting on a child's output or exit.
pub(crate) const POLL: Duration = Duration::from_millis(100);

/// A spawned child killed and reaped when it goes out of scope.
///
/// `Drop` is the only teardown that survives a panic or an early `?`, which is
/// why nothing here relies on a tidy end-of-function `kill()`.
#[derive(Debug)]
pub(crate) struct Proc {
    child: Child,
}

impl Proc {
    /// Ask the child to shut down the way a user's Ctrl-C would, then wait up
    /// to [`SHUTDOWN_GRACE`] for it to finish its own cleanup.
    ///
    /// This matters for the mount consumer specifically: `SIGKILL` would leave
    /// the NFS mount in the table, because the unmount runs *after* its
    /// `ctrl_c()` await returns.
    pub(crate) fn interrupt(&mut self) {
        self.signal_int();
        let deadline = Instant::now() + SHUTDOWN_GRACE;
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => thread::sleep(POLL),
                Err(_) => break,
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// Cumulative CPU seconds this process has burned so far.
    ///
    /// Sampled with `ps` rather than `getrusage`, which has no safe std
    /// wrapper and would need an `unsafe` block the workspace denies. Callers
    /// take a delta across the measurement window so the process's startup and
    /// idle time do not land in the number.
    pub(crate) fn cpu_seconds(&self) -> Option<f64> {
        let output = Command::new("ps")
            .args(["-o", "time=", "-p", &self.child.id().to_string()])
            .output()
            .ok()?;
        parse_ps_time(String::from_utf8_lossy(&output.stdout).trim())
    }

    /// Send `SIGINT` without an `unsafe` `libc::kill`: the workspace denies
    /// `unsafe_code`, and shelling out to `kill(1)` costs a fork we only pay
    /// once per process teardown.
    fn signal_int(&self) {
        if cfg!(unix) {
            let _ = Command::new("/bin/kill")
                .arg("-INT")
                .arg(self.child.id().to_string())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        super::reap::untrack_pid(self.child.id());
    }
}

/// A child's stdout, delivered line by line off a reader thread.
///
/// The thread exists so a chatty child can never block on a full pipe buffer
/// while the harness is busy timing something else.
#[derive(Debug)]
pub(crate) struct Lines {
    rx: Receiver<String>,
    seen: Vec<String>,
}

impl Lines {
    /// Wait up to `timeout` for a line containing `needle`, returning it whole.
    ///
    /// Lines that do not match are retained, so a later `wait_for` can still
    /// find something that arrived early.
    pub(crate) fn wait_for(&mut self, needle: &str, timeout: Duration) -> Option<String> {
        if let Some(found) = self.seen.iter().find(|line| line.contains(needle)) {
            return Some(found.clone());
        }
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            match self.rx.recv_timeout(POLL) {
                Ok(line) => {
                    let matched = line.contains(needle);
                    self.seen.push(line);
                    if matched {
                        return self.seen.last().cloned();
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return None,
            }
        }
        None
    }

    /// Everything seen so far, for an error message when `wait_for` gives up.
    pub(crate) fn transcript(&mut self) -> String {
        while let Ok(line) = self.rx.try_recv() {
            self.seen.push(line);
        }
        if self.seen.is_empty() {
            return "<no output>".to_owned();
        }
        self.seen.join("\n")
    }
}

/// Spawn `cmd` with stdout piped onto a [`Lines`] channel.
///
/// stderr is inherited rather than captured: it carries `tracing` output that
/// is useful to see when a cell hangs, and nothing the harness parses.
pub(crate) fn spawn_piped(mut cmd: Command, label: &str) -> Res<(Proc, Lines)> {
    // The marker a stale-run reaper matches this pid against: the program we
    // asked for, so a recycled pid running something else is left alone.
    let marker = cmd.get_program().to_string_lossy().into_owned();
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("failed to spawn {label}: {error}"))?;
    super::reap::track_pid(child.id(), &marker);
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("{label} produced no stdout handle"))?;

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

    Ok((
        Proc { child },
        Lines {
            rx,
            seen: Vec::new(),
        },
    ))
}

/// What a run-to-completion child produced.
#[derive(Debug)]
pub(crate) struct Captured {
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) status: ExitStatus,
}

/// Run `cmd` to completion, capturing both streams, killing it past `timeout`.
///
/// `Child::wait_with_output` would be simpler but takes the child by value, so
/// there would be nothing left to kill when the deadline passes. Draining both
/// pipes on their own threads keeps `try_wait` polling in this one.
pub(crate) fn run_capture(mut cmd: Command, label: &str, timeout: Duration) -> Res<Captured> {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("failed to spawn {label}: {error}"))?;

    let out_thread = drain(child.stdout.take());
    let err_thread = drain(child.stderr.take());

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => return Err(format!("waiting on {label} failed: {error}").into()),
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("{label} did not finish within {}s", timeout.as_secs()).into());
        }
        thread::sleep(POLL);
    };

    Ok(Captured {
        stdout: join(out_thread),
        stderr: join(err_thread),
        status,
    })
}

/// Read a pipe to end on its own thread so neither stream can deadlock the
/// other by filling its buffer while we wait on the wrong one.
fn drain<R: Read + Send + 'static>(handle: Option<R>) -> Option<JoinHandle<String>> {
    let mut reader = handle?;
    Some(thread::spawn(move || {
        let mut buffer = String::new();
        let _ = reader.read_to_string(&mut buffer);
        buffer
    }))
}

fn join(handle: Option<JoinHandle<String>>) -> String {
    handle
        .and_then(|thread| thread.join().ok())
        .unwrap_or_default()
}

/// Build a command that reports its child's CPU split on stderr.
///
/// `/usr/bin/time -p` prints POSIX `real`/`user`/`sys` lines to stderr and
/// leaves stdout untouched, so a child whose product *is* stdout — the bench
/// consumer's JSON report — still parses. The bool says whether the wrapper is
/// actually in play; without it CPU is reported as unmeasured rather than zero.
pub(crate) fn timed(program: &str, args: &[&str]) -> (Command, bool) {
    if Path::new("/usr/bin/time").exists() {
        let mut cmd = Command::new("/usr/bin/time");
        cmd.arg("-p").arg(program).args(args);
        return (cmd, true);
    }
    let mut cmd = Command::new(program);
    cmd.args(args);
    (cmd, false)
}

/// Pull `(user, sys)` seconds out of `/usr/bin/time -p` output.
///
/// The lines are `user 1.20` — a label, whitespace, a float. Anything else is
/// the child's own stderr and is ignored.
pub(crate) fn parse_posix_time(stderr: &str) -> Option<(f64, f64)> {
    let field = |name: &str| -> Option<f64> {
        stderr.lines().rev().find_map(|line| {
            let rest = line.trim().strip_prefix(name)?;
            rest.trim().parse::<f64>().ok()
        })
    };
    Some((field("user")?, field("sys")?))
}

/// Parse `ps -o time=` — `[[hh:]mm:]ss[.ff]`, most significant field first.
pub(crate) fn parse_ps_time(text: &str) -> Option<f64> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut seconds: f64 = 0.0;
    for field in trimmed.split(':') {
        let value: f64 = field.trim().parse().ok()?;
        seconds = seconds.mul_add(60.0, value);
    }
    Some(seconds)
}

/// A throwaway directory under the OS temp dir, removed recursively on drop.
///
/// The repo has no `tempfile` dependency and deliberately so — `tests/mount.rs`
/// hand-rolls the same thing.
#[derive(Debug)]
pub(crate) struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub(crate) fn new(tag: &str) -> Res<Self> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "agent-share-bench-{}-{tag}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path)
            .map_err(|error| format!("create temp dir {}: {error}", path.display()))?;
        Ok(Self { path })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
