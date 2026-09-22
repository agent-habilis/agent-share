//! `cargo task bench` — the performance matrix, with nobody in the loop.
//!
//! `docs/rfc/02-performance.md` forbids landing any performance change before a
//! baseline exists, and until now no baseline could be produced without a human
//! running two shells or clicking two panels on `/lab`. Everything this task
//! drives already existed; none of it was ever scripted.
//!
//! Two rules the RFC is emphatic about, both enforced here:
//!
//! 1. **Provenance or it did not happen.** Missing provenance is what made the
//!    previous, deleted numbers untrustworthy.
//! 2. **A skipped cell is still a row.** Silent truncation reads as "covered
//!    everything" when it did not.
//!
//! RTT is *measured*, not injected: every row carries its own
//! `latency_ms.median`, which is why this needs no `dnctl`/`pfctl` and no root.

use std::process::Command;
use std::time::{Duration, Instant};

use xshell::{Shell, cmd};

use self::proc::{Proc, Res, run_capture, spawn_piped};
use self::row::{BenchReport, Provenance, Report, Row, SPREAD_WARN_PCT};
use crate::TaskOutcome;
use crate::util::{self, output};

pub(crate) mod browser;
pub(crate) mod mount;
pub(crate) mod proc;
pub(crate) mod reap;
pub(crate) mod row;

/// A swarm id with loopback lookups (no mDNS/DHT/relay), so a loopback cell
/// stays on this host. Same constant the subprocess tests pin
/// (`crates/agent-share/tests/common/mod.rs:21`); duplicated rather than shared
/// because that module is test-local to another crate.
const LOOPBACK_SWARM_ID: &str = "2UXAThUkdBAbiJNXvCt4YeMGQ9myFg7gJJZSr3pG3MAGzUwWmmV7D2Msw3sco";

/// The built artifact the browser leg serves, and the size finding #4 tracks.
/// `scripts/build-wasm.ts` writes wasm-bindgen's output into the package that
/// consumes it, so this is where the browser target lands rather than the
/// crate's own `dist/`.
pub(crate) const WASM_ARTIFACT: &str =
    "packages/agent-share-wasm/src/glue/agent_share_wasm_client_bg.wasm";

/// How long to wait for a producer to print its ticket.
const TICKET_TIMEOUT: Duration = Duration::from_mins(1);

/// Slack on top of the bench window: the consumer's `dial_with_retry` alone
/// allows 90 s (`mount/bench.rs:450`) before it gives up.
const DIAL_SLACK: Duration = Duration::from_secs(150);

/// Knobs, mirrored from the `Bench` variant in `main.rs`.
#[derive(Debug, Clone)]
pub(crate) struct Options {
    pub(crate) duration: u64,
    pub(crate) repeats: usize,
    pub(crate) corpus_mib: u64,
    pub(crate) cells: String,
    pub(crate) tag: String,
    /// Fill depths to sweep for the synthetic cells. `[1]` reproduces the
    /// committed baseline; more values add one row per depth.
    pub(crate) depths: Vec<usize>,
}

pub(crate) fn run(sh: &Shell, opts: &Options) -> TaskOutcome {
    if opts.repeats == 0 {
        return Err("--repeats must be at least 1".into());
    }
    // A signal kills this process without running destructors, so the previous
    // run — not this one — is where an interrupted teardown gets finished.
    reap::reap_stale();
    let binary = util::build_binary(sh, util::Profile::Release)?;

    let mut rows = Vec::new();
    for transport in ["quic", "webrtc"] {
        if !wants(&opts.cells, &format!("native-synth-{transport}")) {
            continue;
        }
        for depth in &opts.depths {
            rows.push(cell_native_synth(&binary, opts, transport, *depth));
        }
    }
    if wants(&opts.cells, "native-mount-cp") {
        rows.push(mount::cell(&binary, opts, None));
    }
    if wants(&opts.cells, "native-mount-cp-webrtc") {
        rows.push(mount::cell(&binary, opts, Some("webrtc")));
    }

    // Batched: one dev server and one headless window across every browser
    // cell, because launching Chrome and a bundler per cell would dominate.
    let browser_cells: Vec<(String, &str, bool)> = [
        ("browser-consume-webrtc", "webrtc", true),
        ("browser-produce-webrtc", "webrtc", false),
    ]
    .into_iter()
    .filter(|(cell, _, _)| wants(&opts.cells, cell))
    .map(|(cell, transport, consume)| (cell.to_owned(), transport, consume))
    .collect();
    rows.extend(browser::cells(&binary, opts, &browser_cells));

    let report = Report {
        provenance: provenance(sh, opts),
        rows: rows.into_iter().map(unwrap_row).collect(),
    };
    warn_on_spread(&report);

    let perf_dir = util::repo_root().join("docs/perf");
    report.write(&perf_dir)?;
    output::status("Finished", &output::home_path(&perf_dir.join("README.md")));
    Ok(())
}

/// A cell that failed outright still produces a row saying so, rather than
/// aborting the run and losing every cell that already succeeded.
fn unwrap_row(result: Result<Row, (String, String, String, String)>) -> Row {
    match result {
        Ok(row) => row,
        Err((cell, transport, direction, reason)) => {
            output::warn(&format!("{cell}: {reason}"));
            Row::skipped(&cell, &transport, &direction, &reason)
        }
    }
}

/// `--cells all` or a comma-separated subset.
fn wants(selector: &str, cell: &str) -> bool {
    selector == "all" || selector.split(',').any(|want| want.trim() == cell)
}

// ---------------------------------------------------------------------------
// Cells
// ---------------------------------------------------------------------------

type CellResult = Result<Row, (String, String, String, String)>;

/// One repeat: the report, plus what it cost both sides.
#[derive(Debug)]
struct Sample {
    report: BenchReport,
    consumer_cpu: Option<(f64, f64)>,
    producer_cpu_s: Option<f64>,
    wall_s: f64,
}

/// Synthetic `OP_BENCH`: `agent-share bench --transport <t>` as producer,
/// `agent-share bench <ticket>` as consumer.
///
/// This is the RFC's "how fast can the CPU go" number. It is deliberately *not*
/// mount throughput — `fill_once` is awaited one at a time
/// (`mount/bench.rs:493`), so it is single-stream serial and must be read as a
/// latency probe and a floor.
fn cell_native_synth(binary: &str, opts: &Options, transport: &str, depth: usize) -> CellResult {
    // Depth 1 keeps the plain name so it stays diffable against the committed
    // baseline; a sweep suffixes the rest rather than renaming everything.
    let cell = if depth == 1 {
        format!("native-synth-{transport}")
    } else {
        format!("native-synth-{transport}-d{depth}")
    };
    let direction = "native->native".to_owned();
    let fail = |reason: String| {
        (
            cell.clone(),
            transport.to_owned(),
            direction.clone(),
            reason,
        )
    };

    output::status("Running", &cell);
    let (mut producer, ticket) =
        start_bench_producer(binary, transport).map_err(|error| fail(error.to_string()))?;

    let mut samples = Vec::new();
    for attempt in 1..=opts.repeats {
        output::status("Sampling", &format!("{cell} {attempt}/{}", opts.repeats));
        let before = producer.cpu_seconds();
        let started = Instant::now();
        let outcome = bench_consume(binary, &ticket, opts.duration, depth);
        let wall_s = started.elapsed().as_secs_f64();
        match outcome {
            Ok((report, consumer_cpu)) => samples.push(Sample {
                report,
                consumer_cpu,
                producer_cpu_s: delta(before, producer.cpu_seconds()),
                wall_s,
            }),
            Err(error) => {
                producer.interrupt();
                return Err(fail(format!("repeat {attempt} failed: {error}")));
            }
        }
    }
    producer.interrupt();

    let mut notes = vec![
        "single-stream serial fill (`mount/bench.rs:493`) — a latency probe and \
         a floor, not mount throughput"
            .to_owned(),
    ];
    if depth > 1 {
        notes.push(
            "at depth > 1 the RTT column measures queueing, not path latency: \
             the echo probe queues behind the in-flight fills"
                .to_owned(),
        );
    }
    if transport == "quic" {
        notes.push(
            "the control leg — plain iroh QUIC over UDP, nothing forced and \
             nothing cleared"
                .to_owned(),
        );
    } else {
        notes.push(
            "QUIC inside a WebRTC data channel: bytes are encrypted twice, and \
             `host/sender.rs:52` undoes GSO batching into one data-channel \
             message per QUIC datagram"
                .to_owned(),
        );
    }
    Ok(finish(&cell, &direction, BENCH_MESH, samples, notes))
}

/// Bench endpoints never call `join_share_mesh`; only the mount path does.
const BENCH_MESH: &str = "none (bench does not join a share mesh)";

/// Fold samples into a row, aligning the CPU figures with the median run so the
/// throughput and the cost in the same row describe the same repeat.
fn finish(
    cell: &str,
    direction: &str,
    mesh: &str,
    mut samples: Vec<Sample>,
    notes: Vec<String>,
) -> Row {
    samples.sort_by(|left, right| {
        left.report
            .throughput_mib_s
            .partial_cmp(&right.report.throughput_mib_s)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let middle = samples.len() / 2;
    let cpu = samples.get(middle).map(|sample| {
        row::Cpu::from_split(sample.consumer_cpu, sample.producer_cpu_s, sample.wall_s)
    });
    let reports = samples.into_iter().map(|sample| sample.report).collect();

    let row = Row::from_reports(cell, direction, mesh, reports, notes);
    cpu.map_or_else(|| row.clone(), |usage| row.clone().with_cpu(usage))
}

fn delta(before: Option<f64>, after: Option<f64>) -> Option<f64> {
    Some((after? - before?).max(0.0))
}

/// Spawn the bench producer and scrape its ticket.
///
/// In `--output json` mode `announce` prints only the bare command
/// (`mount/mod.rs:96-99`), which for bench is `agent-share bench <ticket>` — so
/// the ticket is the third word.
fn start_bench_producer(binary: &str, transport: &str) -> Res<(Proc, String)> {
    let mut cmd = Command::new(binary);
    cmd.args(["bench", "--transport", transport, "--output", "json"]);
    let (producer, mut lines) = spawn_piped(cmd, "bench producer")?;

    let Some(line) = lines.wait_for("agent-share bench", TICKET_TIMEOUT) else {
        return Err(format!(
            "producer never printed a ticket within {}s; output was:\n{}",
            TICKET_TIMEOUT.as_secs(),
            lines.transcript()
        )
        .into());
    };
    let ticket = line
        .split_whitespace()
        .nth(2)
        .ok_or_else(|| format!("ticket line has no ticket token: {line}"))?
        .to_owned();
    // Same hazard as the dev server: the producer keeps printing after its
    // ticket line, and a closed pipe would kill it mid-cell. See
    // `Lines::drain_in_background`.
    lines.drain_in_background();
    Ok((producer, ticket))
}

/// Run one bench consumer to completion; return its report and CPU split.
fn bench_consume(
    binary: &str,
    ticket: &str,
    duration: u64,
    depth: usize,
) -> Res<(BenchReport, Option<(f64, f64)>)> {
    let secs = duration.to_string();
    let depth_arg = depth.to_string();
    let (cmd, timed_run) = proc::timed(
        binary,
        &[
            "bench",
            ticket,
            "--duration",
            &secs,
            "--depth",
            &depth_arg,
            "--output",
            "json",
        ],
    );
    let timeout = Duration::from_secs(duration) + DIAL_SLACK;
    let captured = run_capture(cmd, "bench consumer", timeout)?;
    if !captured.status.success() {
        return Err(format!(
            "bench consumer exited {}: {}",
            captured.status,
            captured.stderr.trim()
        )
        .into());
    }
    let report = serde_json::from_str(&captured.stdout).map_err(|error| {
        format!(
            "could not parse the bench report ({error}); stdout was:\n{}",
            captured.stdout.trim()
        )
    })?;
    let cpu = timed_run
        .then(|| proc::parse_posix_time(&captured.stderr))
        .flatten();
    Ok((report, cpu))
}

// ---------------------------------------------------------------------------
// Provenance
// ---------------------------------------------------------------------------

fn provenance(sh: &Shell, opts: &Options) -> Provenance {
    Provenance {
        tag: opts.tag.clone(),
        git_sha: read(sh, "git rev-parse --short HEAD"),
        git_dirty: !read(sh, "git status --porcelain").is_empty(),
        machine: machine(sh),
        os: os_version(sh),
        rustc: read(sh, "rustc --version"),
        chrome: chrome_version(),
        wasm_bytes: std::fs::metadata(util::repo_root().join(WASM_ARTIFACT))
            .ok()
            .map(|meta| meta.len()),
        corpus_mib: opts.corpus_mib,
        duration_s: opts.duration,
        repeats: opts.repeats,
    }
}

fn machine(sh: &Shell) -> String {
    let model = read(sh, "sysctl -n hw.model");
    if model.is_empty() {
        return read(sh, "uname -m");
    }
    let cores = read(sh, "sysctl -n hw.ncpu");
    format!("{model} ({cores} cores)")
}

fn os_version(sh: &Shell) -> String {
    let name = read(sh, "sw_vers -productName");
    if name.is_empty() {
        return read(sh, "uname -sr");
    }
    format!("{name} {}", read(sh, "sw_vers -productVersion"))
}

/// `None` when `agent-browse` is not installed — the browser cells then record
/// a reasoned skip rather than failing the run. Mirrors the optional-tool probe
/// `ci::wasm_clang` already uses.
///
/// Not `xshell`'s `read()`: `agent-browse status` reports on **stderr** (it is
/// a cargo-style status block, not a machine product), so reading stdout alone
/// finds nothing and leaks the block into our own output.
pub(crate) fn chrome_version() -> Option<String> {
    let output = Command::new("agent-browse").arg("status").output().ok()?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    text.lines()
        .find_map(|line| line.trim().strip_prefix("Chrome version:"))
        .map(|version| version.trim().to_owned())
}

/// Read a command's trimmed stdout, or an empty string if it is unavailable —
/// provenance is best-effort per field, never fatal.
fn read(sh: &Shell, line: &str) -> String {
    let mut parts = line.split_whitespace();
    let Some(program) = parts.next() else {
        return String::new();
    };
    let args: Vec<&str> = parts.collect();
    cmd!(sh, "{program} {args...}")
        .quiet()
        .ignore_status()
        .read()
        .map(|text| text.trim().to_owned())
        .unwrap_or_default()
}

/// The RFC wants three runs "reproducible within noise"; say so out loud when
/// they are not, rather than reporting a median nobody can reproduce.
fn warn_on_spread(report: &Report) {
    for row in &report.rows {
        if let Some(spread) = row.spread_pct
            && spread > SPREAD_WARN_PCT
        {
            output::warn(&format!(
                "{}: {spread:.0}% spread across {} runs — treat the median as indicative only",
                row.cell,
                row.samples_mib_s.len()
            ));
        }
    }
}
