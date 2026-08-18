//! The browser cells: a real headless Chrome driving `/lab`.
//!
//! No application code changes to make this drivable. `/lab` is click-driven,
//! but it is built on stable committed DOM ids (`packages/agent-share-app/src/lab/consumer.tsx`,
//! `packages/agent-share-app/src/lab/producer.tsx`) and `runBench` already ends with
//! `log('report', report)` — and the log writer (`packages/agent-share-app/src/lab/parts.tsx`)
//! `JSON.stringify`s any non-string. So the `BenchReport` is already sitting in
//! `#rx-log` as JSON; nobody had read it.
//!
//! Chrome rather than Node deliberately: `node-datachannel` is `libdatachannel`,
//! a different WebRTC implementation, and the backpressure question in
//! `docs/rfc/02-performance.md` is specifically about `bufferedAmount`
//! semantics, which would not transfer. Chrome also has the main thread the
//! JS↔wasm findings live on.

use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use super::proc::{POLL, Proc, Res, spawn_piped_with_stderr};
use super::row::{BenchReport, Cpu, Row};
use super::{CellResult, Options, WASM_ARTIFACT};
use crate::util::{self, output};

/// The browser's bench window is fixed: `lab/consumer.tsx` passes `undefined` for
/// duration, so `DEFAULT_BENCH_DURATION_SECS` applies and `--duration` cannot
/// reach it without an app change.
const BROWSER_WINDOW: Duration = Duration::from_secs(30);

/// Connect over a relay-brokered `WebRTC` handshake measured at ~10 s from the
/// browser, so the window alone is nowhere near enough.
const REPORT_TIMEOUT: Duration = Duration::from_mins(3);

/// How long to wait for the producer panel to mint a ticket.
const TICKET_TIMEOUT: Duration = Duration::from_secs(90);

const MESH: &str = "none (bench does not join a share mesh)";

/// Quits the headless window when the cells are done, however they end.
#[derive(Debug)]
pub(crate) struct Browser {
    folder: String,
}

impl Browser {
    /// Take ownership of an already-launched window, so it is quit on drop.
    ///
    /// Separate from launching on purpose: `e2e` launches one window per cell
    /// at its own URL, and the guard is what makes a failed cell tear its
    /// window down rather than leave it for the next one to inherit.
    pub(crate) fn new(folder: String) -> Self {
        Self { folder }
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        let _ = Command::new("agent-browse")
            .args(["quit", &self.folder])
            .output();
        super::reap::untrack_browser(&self.folder);
    }
}

/// Run every browser cell selected in `opts`, sharing one server and one window.
///
/// Returned as a batch rather than one call per cell because launching Chrome
/// and a bundler per cell would dominate the runtime.
pub(crate) fn cells(
    binary: &str,
    opts: &Options,
    wanted: &[(String, &str, bool)],
) -> Vec<CellResult> {
    if wanted.is_empty() {
        return Vec::new();
    }
    match prepare() {
        Err(reason) => wanted
            .iter()
            .map(|(cell, transport, _)| {
                Err((
                    cell.clone(),
                    (*transport).to_owned(),
                    "browser".to_owned(),
                    reason.clone(),
                ))
            })
            .collect(),
        Ok((_server, _browser)) => wanted
            .iter()
            .map(|(cell, transport, consume)| {
                let outcome = if *consume {
                    consume_cell(binary, opts, cell, transport)
                } else {
                    produce_cell(binary, opts, cell, transport)
                };
                outcome.map_err(|error| {
                    let direction = if *consume {
                        "native->browser"
                    } else {
                        "browser->native"
                    };
                    (
                        cell.clone(),
                        (*transport).to_owned(),
                        direction.to_owned(),
                        error.to_string(),
                    )
                })
            })
            .inspect(|outcome| {
                if let Err((cell, _, _, reason)) = outcome {
                    output::warn(&format!("{cell}: {reason}"));
                }
            })
            .collect(),
        // `_browser` drops at the end of this arm, quitting the window.
    }
}

/// Start the dev server and land a headless window on `/lab`.
///
/// `scripts/dev.ts` rather than `scripts/start.ts`: it needs only the wasm dist, where
/// the prod server would additionally need `bun run build`. The `.wasm` is byte-identical
/// either way — only the JS glue's bundling differs, which matters for finding
/// #4's load time but not for throughput.
fn prepare() -> Result<(Proc, Browser), String> {
    if Command::new("agent-browse")
        .arg("--version")
        .output()
        .is_err()
    {
        return Err("agent-browse is not installed — the browser cells need it".to_owned());
    }
    let root = util::repo_root();
    if !root.join(WASM_ARTIFACT).exists() {
        return Err("the wasm client is missing — run `cargo task web-wasm`".to_owned());
    }

    let (server, url) = start_dev_server(&root).map_err(|error| error.to_string())?;
    let folder = root.display().to_string();
    let lab = format!("{url}lab");
    output::status("Launching", &format!("headless chrome on {lab}"));

    run_browse(&["launch", "--headless", &folder, &lab])
        .map_err(|error| format!("launching headless chrome failed: {error}"))?;
    super::reap::track_browser(&folder);
    let browser = Browser {
        folder: folder.clone(),
    };

    // Drop Chrome's HTTP cache, then reload.
    //
    // `scripts/dev.ts` serves the `.wasm` straight from the crate's dist, so a
    // rebuilt binary Chrome has already fetched can come back from its cache —
    // while Bun re-bundles the JS glue fresh. A stale wasm
    // against new glue fails instantiation on a mismatched
    // `__wbindgen_cast_*` import, which is how the opt-level canary caught it.
    //
    // `clearBrowserCache`, not `setCacheDisabled`: the latter is scoped to the
    // CDP *session*, and each `agent-browse cdp` call is its own short-lived
    // session, so it is undone before the reload in the next call. Clearing is
    // a one-shot effect that outlives the session that asked for it.
    run_browse(&[
        "cdp",
        "Network.clearBrowserCache",
        "{}",
        "--folder",
        &folder,
    ])
    .map_err(|error| format!("clearing the browser cache failed: {error}"))?;
    run_browse(&[
        "cdp",
        "Page.reload",
        "{\"ignoreCache\":true}",
        "--folder",
        &folder,
    ])
    .map_err(|error| format!("reloading /lab failed: {error}"))?;

    run_browse(&[
        "wait",
        "--selector",
        "#rx-run",
        "--timeout",
        "60000",
        "--folder",
        &folder,
    ])
    .map_err(|error| format!("/lab never became interactive: {error}"))?;
    Ok((server, browser))
}

/// Start a dev server on a port nobody else can be holding.
///
/// `PORT=0` rather than `scripts/dev.ts`'s default 3000, and that is not a nicety. The
/// default collided with a *sibling worktree's* dev server, which re-took the
/// port within seconds of being freed — so the harness alternated between
/// `EADDRINUSE` and, worse, adopting a server that was serving another
/// checkout's build. A fixed port makes every concurrent checkout, and the
/// human's own `bun run dev`, a contender for the same socket.
///
/// `scripts/dev.ts` prints the URL it actually bound (`dev ${server.url}`), so the
/// ephemeral port costs nothing to discover.
pub(crate) fn start_dev_server(root: &Path) -> Res<(Proc, String)> {
    let mut cmd = Command::new("bun");
    cmd.arg("scripts/dev.ts").current_dir(root).env("PORT", "0");
    let (server, mut lines) = spawn_piped_with_stderr(cmd, "bun dev server")?;
    // `scripts/dev.ts` prints `dev http://localhost:3000/` once bound.
    let Some(line) = lines.wait_for("dev http", Duration::from_mins(1)) else {
        return Err(format!(
            "the dev server never reported a URL; output was:\n{}",
            lines.transcript()
        )
        .into());
    };
    let url = line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| format!("dev server line has no URL: {line}"))?
        .to_owned();
    assert_wasm_is_fresh(root, &url)?;
    // The server keeps logging for the rest of the run, and something has to
    // keep reading or it takes a `SIGPIPE` on the next line it writes.
    lines.drain_in_background();
    Ok((server, url))
}

/// Refuse to test a build the server is not actually serving.
///
/// Twice in one session a mismatched wasm masqueraded as a product bug — once
/// as `CompileError: … Custom section … would overflow Module's size`, once as
/// `decode ticket: ticket address truncated`, a string present in neither the
/// source nor the binary. Both times the harness looked like it was exercising
/// the change and was not, and both cost far more than this check.
///
/// Compares length rather than content: the failure mode is serving a
/// *different build*, which never has the same size, and reading 7 MB twice per
/// run to catch a same-size difference that cannot happen is not worth it.
fn assert_wasm_is_fresh(root: &Path, url: &str) -> Res<()> {
    let on_disk = std::fs::metadata(root.join(WASM_ARTIFACT))
        .map_err(|error| format!("cannot stat the built wasm: {error}"))?
        .len();
    let path = served_wasm_path(root)?;
    let served = Command::new("curl")
        .args(["-s", "-o", "/dev/null", "-w", "%{size_download}"])
        .arg(format!("{}{}", url.trim_end_matches('/'), path))
        .output()
        .map_err(|error| format!("cannot fetch the served wasm: {error}"))?;
    let served: u64 = String::from_utf8_lossy(&served.stdout)
        .trim()
        .parse()
        .map_err(|_| "the dev server did not return a wasm".to_owned())?;
    if served != on_disk {
        return Err(format!(
            "the dev server is serving a different wasm than the one on disk \
             ({served} vs {on_disk} bytes). Something else is bound to this \
             port, or the server predates the last `cargo task web-wasm`. \
             Testing would report on a build that is not the one you changed."
        )
        .into());
    }
    Ok(())
}

/// The URL path the dev server actually answers the wasm on.
///
/// Content-addressed, so it cannot be a constant here: `scripts/wasm-asset.ts`
/// hashes the binary and writes the path into `packages/agent-share-wasm/src/path.ts`, and the
/// dev server 404s the old fixed name on purpose. Read from that generated file
/// rather than re-deriving the hash, so there is one source of truth and no
/// second implementation of the digest to drift.
///
/// Safe to read at this point in the run: `scripts/dev.ts` regenerates it before it
/// binds a port, and the caller has already seen the server's ready line.
fn served_wasm_path(root: &Path) -> Res<String> {
    const GENERATED: &str = "packages/agent-share-wasm/src/path.ts";
    let source = std::fs::read_to_string(root.join(GENERATED))
        .map_err(|error| format!("cannot read {GENERATED}: {error}"))?;
    source
        .split_once("WASM_PATH = '")
        .and_then(|(_, rest)| rest.split_once('\''))
        .map(|(path, _)| path.to_owned())
        .ok_or_else(|| format!("{GENERATED} has no WASM_PATH literal to read").into())
}

/// Browser as consumer, native `agent-share bench` as producer.
fn consume_cell(binary: &str, opts: &Options, cell: &str, transport: &str) -> Res<Row> {
    output::status("Running", cell);
    let (mut producer, ticket) = super::start_bench_producer(binary, transport)?;

    let mut reports = Vec::new();
    let mut cpu = None;
    for attempt in 1..=opts.repeats {
        output::status("Sampling", &format!("{cell} {attempt}/{}", opts.repeats));
        let before = producer.cpu_seconds();
        let started = Instant::now();
        match run_browser_bench(&ticket) {
            Ok(report) => {
                cpu = Some(Cpu::from_totals(
                    None,
                    super::delta(before, producer.cpu_seconds()),
                    started.elapsed().as_secs_f64(),
                ));
                reports.push(report);
            }
            Err(error) => {
                producer.interrupt();
                return Err(format!("repeat {attempt} failed: {error}").into());
            }
        }
    }
    producer.interrupt();
    Ok(finish(cell, "native->browser", reports, cpu))
}

/// Browser as producer (`BenchProducer`), native `agent-share bench` as consumer.
///
/// Only the *synthetic* producer is reachable this way. A browser serving real
/// files needs the File System Access picker, which requires a user gesture —
/// so the `read_from_handle` path stays manual.
fn produce_cell(binary: &str, opts: &Options, cell: &str, transport: &str) -> Res<Row> {
    output::status("Running", cell);
    let ticket = start_browser_producer(transport)?;

    let mut reports = Vec::new();
    for attempt in 1..=opts.repeats {
        output::status("Sampling", &format!("{cell} {attempt}/{}", opts.repeats));
        let (report, _) = super::bench_consume(binary, &ticket, opts.duration, 1)?;
        reports.push(report);
    }
    let _ = evaluate("document.getElementById('tx-stop').click(), 'stopped'");
    Ok(finish(cell, "browser->native", reports, None))
}

fn finish(cell: &str, direction: &str, reports: Vec<BenchReport>, cpu: Option<Cpu>) -> Row {
    let mut notes = vec![
        "the bench window is pinned at 30 s — `lab/consumer.tsx` passes `undefined` \
         for duration, so `--duration` does not reach browser cells"
            .to_owned(),
        "signalling is brokered over the iroh relay before the direct data \
         channel opens, so this row needs network reach and its `connect (ms)` \
         includes that hop"
            .to_owned(),
    ];
    if reports.iter().any(|report| report.latency_ms.median == 0.0) {
        notes.push(
            "RTT reads 0: the browser's `performance.now()` is clamped, so the \
             wasm bench cannot resolve a sub-millisecond round trip. Treat the \
             RTT column as unmeasured here, not as zero"
                .to_owned(),
        );
    }
    if cpu.is_some() {
        notes.push(
            "`cores busy` counts the native peer only — Chrome's work is spread \
             across its own process tree and is not attributed here"
                .to_owned(),
        );
    }
    let row = Row::from_reports(cell, direction, MESH, reports, notes);
    cpu.map_or_else(|| row.clone(), |usage| row.clone().with_cpu(usage))
}

/// Paste a ticket into the consumer panel, run, and wait for the report line.
fn run_browser_bench(ticket: &str) -> Res<BenchReport> {
    // Clear the log first: a previous repeat's `report` line is still in the
    // `<pre>` and would be matched again immediately.
    evaluate("document.getElementById('rx-log').textContent = '', 'cleared'")?;
    evaluate(&format!(
        "(()=>{{const box=document.getElementById('rx-ticket');box.value={};\
         document.getElementById('rx-run').click();return 'started'}})()",
        js_string(ticket)
    ))?;

    let deadline = Instant::now() + REPORT_TIMEOUT;
    while Instant::now() < deadline {
        thread::sleep(POLL * 5);
        let log = evaluate("document.getElementById('rx-log').textContent")?;
        if let Some(report) = parse_report(&log) {
            return report;
        }
        if let Some(line) = log.lines().find(|line| line.contains("FAILED")) {
            return Err(format!("the browser reported a failure: {}", line.trim()).into());
        }
    }
    Err(format!(
        "no report within {}s (bench window is {}s)",
        REPORT_TIMEOUT.as_secs(),
        BROWSER_WINDOW.as_secs()
    )
    .into())
}

/// Start the browser's `BenchProducer` and read the ticket it mints.
fn start_browser_producer(transport: &str) -> Res<String> {
    evaluate(&format!(
        "(()=>{{document.getElementById('tx-transport').value={};\
         document.getElementById('tx-start').click();return 'starting'}})()",
        js_string(transport)
    ))?;

    let deadline = Instant::now() + TICKET_TIMEOUT;
    while Instant::now() < deadline {
        thread::sleep(POLL * 5);
        let ticket = evaluate("document.getElementById('tx-ticket').value")?;
        if !ticket.trim().is_empty() {
            return Ok(ticket.trim().to_owned());
        }
        let log = evaluate("document.getElementById('tx-log').textContent")?;
        if let Some(line) = log.lines().find(|line| line.contains("FAILED")) {
            return Err(format!("the browser producer failed: {}", line.trim()).into());
        }
    }
    Err("the browser producer never minted a ticket".into())
}

/// Find the `report {…}` line and parse the JSON after it.
fn parse_report(log: &str) -> Option<Res<BenchReport>> {
    let line = log.lines().find(|line| line.contains(" report {"))?;
    let json = &line[line.find(" report {")? + " report ".len()..];
    Some(
        serde_json::from_str(json).map_err(|error| {
            format!("could not parse the browser report ({error}): {json}").into()
        }),
    )
}

/// Evaluate an expression in the page and return its string result.
///
/// Drives whichever window `agent-browse` finds for the *current directory*,
/// which is the repo root and so the one window a single-peer cell opened. A
/// cell that opens two must say which one it means — see [`evaluate_in`].
pub(crate) fn evaluate(expression: &str) -> Res<String> {
    evaluate_at(expression, None)
}

/// [`evaluate`], against the window keyed to `folder`.
pub(crate) fn evaluate_in(folder: &str, expression: &str) -> Res<String> {
    evaluate_at(expression, Some(folder))
}

fn evaluate_at(expression: &str, folder: Option<&str>) -> Res<String> {
    let params = serde_json::json!({
        "expression": expression,
        "returnByValue": true,
        "awaitPromise": true,
    })
    .to_string();
    let mut args = vec!["cdp", "Runtime.evaluate", &params];
    if let Some(folder) = folder {
        args.extend_from_slice(&["--folder", folder]);
    }
    let raw = run_browse(&args)?;
    let parsed: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|error| format!("agent-browse returned non-JSON ({error}): {raw}"))?;
    if let Some(details) = parsed.get("exceptionDetails") {
        return Err(format!("the page threw while evaluating: {details}").into());
    }
    Ok(parsed
        .pointer("/result/value")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_owned())
}

pub(crate) fn run_browse(args: &[&str]) -> Res<String> {
    let output = Command::new("agent-browse")
        .args(args)
        .output()
        .map_err(|error| format!("running agent-browse {}: {error}", args.join(" ")))?;
    if !output.status.success() {
        return Err(format!(
            "agent-browse {} exited {}: {}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// A JSON string literal — safe to paste into an injected expression.
pub(crate) fn js_string(value: &str) -> String {
    serde_json::Value::String(value.to_owned()).to_string()
}
