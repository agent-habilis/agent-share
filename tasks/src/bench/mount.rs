//! The `native-mount-cp` cell: a real `serve` → OS NFS mount → full read.
//!
//! `docs/rfc/02-performance.md` is emphatic that this number, not
//! `agent-share bench`, is the one comparable to what a user experiences —
//! "nobody has recorded a `serve` → mount → `cp` of a large file", and "if they
//! disagree, say so; do not quote the flattering one".
//!
//! It needs no privileges on macOS: `try_mount` (`mount/consume.rs:627-651`)
//! runs `mount_nfs` as the current user with no `sudo` prefix, and only Linux
//! gets the sudo hint (`consume.rs:124-126`). On anything else the cell records
//! a reasoned skip rather than vanishing.

use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use super::proc::{Proc, Res, TempDir, spawn_piped};
use super::row::{Cpu, Row};
use super::{CellResult, LOOPBACK_SWARM_ID, Options, TICKET_TIMEOUT};
use crate::util::output;

const DIRECTION: &str = "native->native (NFS)";
/// The mount path *does* join a share mesh, on the same endpoint as the data
/// path (`mount/produce.rs:124-127`) — so this row cannot claim a quiet mesh.
const MESH: &str = "share mesh joined — shares the data endpoint and congestion domain";

/// Mounting, connecting and the first read all happen before any bytes move.
const MOUNT_TIMEOUT: Duration = Duration::from_mins(2);

/// Written once and read `repeats` times through fresh mounts.
const CORPUS_FILE: &str = "blob.bin";

/// Run the cell over `transport`: `None` is the default path (plain iroh QUIC
/// over UDP), `Some("webrtc")` forces the same mount through the WebRTC lane.
///
/// Running both is the clean experiment the synthetic cells cannot give: same
/// NFS client, same `rsize`, same producer code, one variable changed. The
/// synthetic bench only offers `--transport webrtc|relay`, so it has no
/// plain-QUIC leg to compare against.
pub(crate) fn cell(binary: &str, opts: &Options, transport: Option<&str>) -> CellResult {
    let cell = transport.map_or_else(
        || "native-mount-cp".to_owned(),
        |name| format!("native-mount-cp-{name}"),
    );
    let label = transport.unwrap_or("quic");
    let fail = |reason: String| (cell.clone(), label.to_owned(), DIRECTION.to_owned(), reason);
    if !cfg!(target_os = "macos") {
        return Err(fail(
            "needs root: only macOS mounts NFS unprivileged (`consume.rs:124-126`)".to_owned(),
        ));
    }

    output::status("Running", &cell);
    let corpus = TempDir::new("corpus").map_err(|error| fail(error.to_string()))?;
    let corpus_bytes = opts.corpus_mib * 1024 * 1024;
    write_corpus(&corpus.path().join(CORPUS_FILE), corpus_bytes)
        .map_err(|error| fail(format!("generating the corpus failed: {error}")))?;

    let (mut producer, ticket) = start_serve(binary, corpus.path(), transport.is_none())
        .map_err(|error| fail(error.to_string()))?;

    let mut samples = Vec::new();
    let mut cpu = None;
    for attempt in 1..=opts.repeats {
        output::status("Sampling", &format!("{cell} {attempt}/{}", opts.repeats));
        // A fresh mount per repeat, so the NFS client's buffer cache is cold
        // every time. Re-reading one mount would measure the cache.
        match one_mount_read(binary, &ticket, corpus_bytes, &producer, transport) {
            Ok(measured) => {
                samples.push(measured.mib_s);
                cpu = Some(measured.cpu);
            }
            Err(error) => {
                producer.interrupt();
                return Err(fail(format!("repeat {attempt} failed: {error}")));
            }
        }
    }
    producer.interrupt();

    let mut notes = vec![
        format!(
            "a full read of one {} MiB file through the OS NFS client",
            opts.corpus_mib
        ),
        "mount options carry `rsize=131072` and no `readahead=` \
         (`consume.rs:588-599`), so this is the depth-1 path finding #1 \
         describes"
            .to_owned(),
    ];
    notes.push(if transport.is_some() {
        "forced onto the WebRTC lane with `--transport webrtc`. Its control is \
         `native-mount-cp`: same NFS client, same `rsize`, same producer code. \
         The pair also differs in discovery — WebRTC signalling needs a relay, \
         so this leg uses the default ladder rather than a loopback swarm — but \
         both move data over a host-local path"
            .to_owned()
    } else {
        "the default path: plain iroh QUIC over UDP, no WebRTC wrapper".to_owned()
    });
    let row = Row::from_transfers(&cell, label, DIRECTION, MESH, samples, corpus_bytes, notes);
    Ok(cpu.map_or_else(|| row.clone(), |usage| row.clone().with_cpu(usage)))
}

/// What one mount-and-read cycle produced.
struct Measured {
    mib_s: f64,
    cpu: Cpu,
}

fn one_mount_read(
    binary: &str,
    ticket: &str,
    corpus_bytes: u64,
    producer: &Proc,
    transport: Option<&str>,
) -> Res<Measured> {
    let target = TempDir::new("mnt")?;
    let mut cmd = Command::new(binary);
    cmd.arg(ticket).arg(target.path());
    if let Some(name) = transport {
        cmd.args(["--transport", name]);
    }
    // Human output on purpose: in `--output json` a *successful* mount prints
    // nothing at all (`consume.rs:111-119` guards the line with `if !json`),
    // so there would be no readiness signal to wait on.
    let (mut consumer, mut lines) = spawn_piped(cmd, "mount consumer")?;

    if lines.wait_for("Mounted", MOUNT_TIMEOUT).is_none() {
        let transcript = lines.transcript();
        consumer.interrupt();
        let reason = if transcript.contains("Bridge") {
            "the OS mount step failed; the bridge bound but nothing was mounted"
        } else {
            "the consumer never reported a mount"
        };
        return Err(format!("{reason}. Output was:\n{transcript}").into());
    }

    let mountpoint = find_mountpoint(target.path())?;
    // Recorded before a single byte is read: if this run is killed mid-transfer
    // the mount outlives us, and only a record on disk can retire it.
    let mount_key = mountpoint.display().to_string();
    super::reap::track_mount(&mount_key);
    let before_producer = producer.cpu_seconds();
    let before_consumer = consumer.cpu_seconds();

    let started = Instant::now();
    let read = read_through(&mountpoint.join(CORPUS_FILE))?;
    let wall_s = started.elapsed().as_secs_f64();

    let cpu = Cpu::from_totals(
        super::delta(before_consumer, consumer.cpu_seconds()),
        super::delta(before_producer, producer.cpu_seconds()),
        wall_s,
    );

    // Interrupt *before* `target` drops: SIGINT is what makes the consumer
    // unmount (`consume.rs:150`), and removing a directory that is still a
    // mountpoint would fail and leave the mount in the table.
    consumer.interrupt();
    super::reap::untrack_mount(&mount_key);

    if read != corpus_bytes {
        return Err(format!("read {read} bytes through the mount, expected {corpus_bytes}").into());
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "corpus sizes are far below 2^52 bytes, where f64 is still exact"
    )]
    let mib_s = if wall_s > 0.0 {
        (read as f64 / (1024.0 * 1024.0)) / wall_s
    } else {
        0.0
    };
    Ok(Measured { mib_s, cpu })
}

/// Stream the file to a sink — the `cp` the RFC asks for, without needing a
/// destination big enough to hold it.
fn read_through(path: &Path) -> Res<u64> {
    let mut file = std::fs::File::open(path)
        .map_err(|error| format!("open {} through the mount: {error}", path.display()))?;
    io::copy(&mut file, &mut io::sink())
        .map_err(|error| format!("read {} through the mount: {error}", path.display()).into())
}

/// The consumer creates `agent-share-<timestamp>/` under the target it was
/// given; that child is the actual mountpoint.
fn find_mountpoint(target: &Path) -> Res<PathBuf> {
    std::fs::read_dir(target)
        .map_err(|error| format!("read {}: {error}", target.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("agent-share-"))
        })
        .ok_or_else(|| format!("no agent-share-* mountpoint under {}", target.display()).into())
}

/// Serve `dir`, on a loopback swarm when `self_contained`.
///
/// The WebRTC lane cannot use one: the browser-style handshake dials
/// `agent-share/webrtc-signal/1` **over the iroh relay** before it opens the
/// direct data connection (README, "How a browser reaches a peer behind NAT"),
/// and a loopback swarm has no relay to reach. That leg therefore needs the
/// default discovery ladder, and therefore the internet.
fn start_serve(binary: &str, dir: &Path, self_contained: bool) -> Res<(Proc, String)> {
    let mut cmd = Command::new(binary);
    cmd.arg("serve").arg(dir);
    if self_contained {
        cmd.args(["--swarm", LOOPBACK_SWARM_ID]);
    }
    cmd.args(["--output", "json"]);
    let (producer, mut lines) = spawn_piped(cmd, "serve producer")?;

    // json mode prints the bare `agent-share <ticket> <mountpoint-hint>`
    // command (`mount/mod.rs:96-99`); the ticket is its second word.
    let Some(line) = lines.wait_for("agent-share", TICKET_TIMEOUT) else {
        return Err(format!(
            "serve never printed a ticket within {}s; output was:\n{}",
            TICKET_TIMEOUT.as_secs(),
            lines.transcript()
        )
        .into());
    };
    let ticket = line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| format!("mount line has no ticket token: {line}"))?
        .to_owned();
    Ok((producer, ticket))
}

/// A corpus of non-repeating bytes.
///
/// Not zeros: nothing in the byte path compresses, but a zero-filled file is
/// exactly the input a sparse-file or dedup optimization would flatter, and the
/// point of this cell is that its number is comparable to a real transfer.
fn write_corpus(path: &Path, bytes: u64) -> Res<()> {
    let file = std::fs::File::create(path)
        .map_err(|error| format!("create {}: {error}", path.display()))?;
    let mut writer = BufWriter::new(file);

    let mut block = vec![0u8; 1024 * 1024];
    let mut state: u32 = 0x9E37_79B9;
    for slot in &mut block {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        *slot = (state >> 24) as u8;
    }

    let mut written = 0u64;
    while written < bytes {
        let want =
            usize::try_from((bytes - written).min(block.len() as u64)).unwrap_or(block.len());
        writer
            .write_all(&block[..want])
            .map_err(|error| format!("write {}: {error}", path.display()))?;
        written += want as u64;
    }
    writer
        .flush()
        .map_err(|error| format!("flush {}: {error}", path.display()))?;
    Ok(())
}
