//! `cargo task e2e` — functional browser cells against a real producer.
//!
//! The gate is strong below the transport (golden pins, snapshots, proptests)
//! and empty above it: nothing renders the app, drives a session, or compares a
//! transferred byte. Every user-visible bug this project hit recently lived in
//! that gap — a connection that never recovered, a zip that opened one read per
//! file at construction, a failure that reached a crash overlay instead of the
//! page.
//!
//! So this is deliberately not a second unit-test layer. Each cell reproduces
//! something that broke, or guards an invariant whose violation would be
//! silent. `docs/testing.md` says which lane covers what, and why.
//!
//! Not part of `cargo task ci`: it needs `agent-browse`, a built wasm, and the
//! network — a browser reaches a native producer by brokering JSEP over the
//! iroh relay before any direct path exists. A missing prerequisite yields a
//! **skipped cell naming the reason**, never a quiet pass, which is the rule
//! the bench harness already follows.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use xshell::{Shell, cmd};

use crate::TaskOutcome;
use crate::bench::browser::{Browser, evaluate, js_string, run_browse, start_dev_server};
use crate::bench::proc::{Proc, Res, TempDir, spawn_piped};
use crate::bench::reap;
use crate::util::{self, output};

/// How long a browser action may take before the cell is called failed.
///
/// Generous on purpose: a browser reaches a native producer over relay-brokered
/// signalling, measured at about ten seconds, and a reconnect pays that again
/// on top of the app's own 60 s reconnect budget.
const ACTION_TIMEOUT: Duration = Duration::from_mins(3);

/// How long to wait for the share view to list its files.
const CONNECT_TIMEOUT: Duration = Duration::from_mins(2);

/// How often to re-read the page while waiting on it.
const POLL: Duration = Duration::from_millis(500);

/// The blob every fixture carries, in bytes.
const BLOB_LEN: usize = 512 * 1024;

/// One cell's verdict. A skip is a result, not an absence.
#[derive(Debug)]
enum Verdict {
    Pass,
    Fail(String),
    Skip(String),
}

#[derive(Debug)]
struct Outcome {
    cell: &'static str,
    verdict: Verdict,
}

/// What every cell needs: the built binary, the dev server, the browser folder.
struct Ctx<'a> {
    binary: &'a str,
    url: &'a str,
    folder: &'a str,
}

/// One row of the matrix.
struct Cell {
    name: &'static str,
    run: fn(&Ctx<'_>) -> Res<()>,
}

/// The matrix, in one place so a skipped run still reports every row.
///
/// A prerequisite missing at the top means nothing can run, and the honest
/// output for that is a skip per row with a reason — not silence, not a pass.
const CELLS: &[Cell] = &[
    Cell {
        name: "web-list",
        run: cell_list,
    },
    Cell {
        name: "web-download-single",
        run: cell_download_single,
    },
    Cell {
        name: "web-download-zip",
        run: cell_download_zip,
    },
    Cell {
        name: "web-download-dismissed",
        run: cell_download_dismissed,
    },
    Cell {
        name: "web-reconnect",
        run: cell_reconnect,
    },
    Cell {
        name: "web-producer-gone",
        run: cell_producer_gone,
    },
    Cell {
        name: "web-transport-webrtc",
        run: cell_transport_webrtc,
    },
    Cell {
        name: "web-transport-relay",
        run: cell_transport_relay,
    },
];

pub(crate) fn run(sh: &Shell, cells: &str) -> TaskOutcome {
    reap::reap_stale();
    let root = util::repo_root();
    let selected = select(cells)?;

    if let Some(reason) = missing_prerequisite(&root) {
        return report(
            &selected
                .iter()
                .map(|cell| Outcome {
                    cell: cell.name,
                    verdict: Verdict::Skip(reason.clone()),
                })
                .collect::<Vec<_>>(),
        );
    }

    let binary = build_binary(sh)?;
    // Starting the server also asserts it is serving the wasm that is on disk,
    // so a stale process holding the port fails here rather than quietly
    // colouring every cell below.
    let (_server, url) = start_dev_server(&root)?;
    let folder = root.display().to_string();
    let ctx = Ctx {
        binary: &binary,
        url: &url,
        folder: &folder,
    };

    let outcomes: Vec<Outcome> = selected
        .iter()
        .map(|cell| {
            output::status("Running", cell.name);
            Outcome {
                cell: cell.name,
                verdict: match (cell.run)(&ctx) {
                    Ok(()) => Verdict::Pass,
                    Err(error) => Verdict::Fail(error.to_string()),
                },
            }
        })
        .collect();

    report(&outcomes)
}

/// Resolve `--cells` to the rows to run.
///
/// An unknown name is an error rather than an empty run: a typo that silently
/// selects nothing reports a clean pass, which is the one outcome a test runner
/// must never invent.
fn select(cells: &str) -> Res<Vec<&'static Cell>> {
    if cells.trim() == "all" {
        return Ok(CELLS.iter().collect());
    }
    cells
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| {
            CELLS.iter().find(|cell| cell.name == name).ok_or_else(|| {
                let known: Vec<&str> = CELLS.iter().map(|cell| cell.name).collect();
                format!(
                    "no such cell `{name}`; known cells are {}",
                    known.join(", ")
                )
                .into()
            })
        })
        .collect()
}

/// The first prerequisite that is not met, if any.
fn missing_prerequisite(root: &Path) -> Option<String> {
    if Command::new("agent-browse")
        .arg("--version")
        .output()
        .is_err()
    {
        return Some("agent-browse is not installed".to_owned());
    }
    if !root.join(crate::bench::WASM_ARTIFACT).exists() {
        return Some("the wasm client is missing — run `cargo task web-wasm`".to_owned());
    }
    if sha256_tool().is_none() {
        return Some("neither `shasum` nor `sha256sum` is on PATH".to_owned());
    }
    if !has_network() {
        return Some(
            "no network — a browser reaches a native producer by brokering \
             signalling over the iroh relay, so every cell needs one"
                .to_owned(),
        );
    }
    None
}

/// A cheap reachability probe, not a relay health check.
///
/// The relay ladder is chosen by iroh at dial time and is not a fixed host this
/// task can poke, so this answers the coarser question it actually needs: is
/// there a route off this machine at all.
fn has_network() -> bool {
    Command::new("curl")
        .args([
            "-sS",
            "-m",
            "8",
            "-o",
            "/dev/null",
            "https://www.google.com/generate_204",
        ])
        .status()
        .is_ok_and(|status| status.success())
}

fn build_binary(sh: &Shell) -> Res<String> {
    output::status("Building", "agent-share (release)");
    cmd!(sh, "cargo build --release -p agent-share")
        .quiet()
        .run()?;
    Ok(util::repo_root()
        .join("target/release/agent-share")
        .display()
        .to_string())
}

fn report(outcomes: &[Outcome]) -> TaskOutcome {
    let mut failed = 0;
    for outcome in outcomes {
        match &outcome.verdict {
            Verdict::Pass => output::status("Passed", outcome.cell),
            Verdict::Skip(reason) => {
                output::status_warn("Skipped", &format!("{}: {reason}", outcome.cell));
            }
            Verdict::Fail(reason) => {
                failed += 1;
                output::error(&format!("{}: {reason}", outcome.cell));
            }
        }
    }
    if failed > 0 {
        return Err(format!("{failed} of {} e2e cells failed", outcomes.len()).into());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The fixture share
// ---------------------------------------------------------------------------

/// A share of `count` small files plus `blob.bin`, whose bytes we can verify.
///
/// The blob is deliberately non-repeating: a transport that dropped or
/// reordered a chunk would still produce the right *length*, which is all the
/// bench harness can see.
fn make_share(count: usize) -> Res<(TempDir, String)> {
    let dir = TempDir::new("e2e")?;
    for index in 0..count {
        std::fs::write(
            dir.path().join(format!("f{index:03}.txt")),
            format!("file {index} contents\n"),
        )
        .map_err(|error| format!("writing the fixture: {error}"))?;
    }

    let mut blob = Vec::with_capacity(BLOB_LEN);
    let mut state: u32 = 0x9E37_79B9;
    for _ in 0..BLOB_LEN {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        blob.push((state >> 24) as u8);
    }
    let path = dir.path().join("blob.bin");
    std::fs::write(&path, &blob).map_err(|error| format!("writing the blob: {error}"))?;

    let digest = sha256_file(&path)?;
    Ok((dir, digest))
}

/// `(program, args)` for whichever SHA-256 tool this host has.
///
/// Shelling out rather than adding a `sha2` dependency: this crate exists to
/// drive other processes, and one hash of one fixture per cell is not worth a
/// new entry in `Cargo.lock`.
fn sha256_tool() -> Option<(&'static str, &'static [&'static str])> {
    for (program, args) in [
        ("shasum", &["-a", "256"] as &[&str]),
        ("sha256sum", &[] as &[&str]),
    ] {
        if Command::new(program).arg("--version").output().is_ok() {
            return Some((program, args));
        }
    }
    None
}

fn sha256_file(path: &Path) -> Res<String> {
    let (program, args) = sha256_tool().ok_or("no SHA-256 tool on PATH")?;
    let output = Command::new(program)
        .args(args)
        .arg(path)
        .output()
        .map_err(|error| format!("hashing {}: {error}", path.display()))?;
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .next()
        .map(str::to_owned)
        .ok_or_else(|| format!("{program} printed no digest for {}", path.display()).into())
}

// ---------------------------------------------------------------------------
// A page under test
// ---------------------------------------------------------------------------

/// One producer, one headless window, one fixture — torn down in that order.
///
/// Field order is drop order and it matters here: the window goes first so
/// nothing is still reading, then the producer, and only then the directory it
/// was serving.
struct Page {
    _browser: Browser,
    producer: Proc,
    _dir: TempDir,
    /// SHA-256 of the fixture's `blob.bin`, for the cells that compare bytes.
    blob_sha256: String,
}

impl Page {
    /// Serve `files + 1` fixture files and land a headless window on the share.
    fn open(ctx: &Ctx<'_>, files: usize, query: &str) -> Res<Self> {
        let (dir, blob_sha256) = make_share(files)?;
        let (producer, ticket) = serve(ctx.binary, dir.path())?;

        let target = format!("{}files/{ticket}{query}", ctx.url);
        run_browse(&["launch", "--headless", ctx.folder, &target])?;
        reap::track_browser(ctx.folder);
        let browser = Browser::new(ctx.folder.to_owned());

        // `dev.ts` serves the wasm at a fixed URL, so a copy cached before the
        // last `cargo task web-wasm` would survive a plain reload. Clearing is
        // a one-shot effect that outlives the short-lived CDP session asking
        // for it, which `setCacheDisabled` would not be.
        run_browse(&[
            "cdp",
            "Network.clearBrowserCache",
            "{}",
            "--folder",
            ctx.folder,
        ])?;
        run_browse(&[
            "cdp",
            "Page.reload",
            "{\"ignoreCache\":true}",
            "--folder",
            ctx.folder,
        ])?;
        // Readiness, not optimism. `launch` returning does not mean the window
        // is drivable, and without this the first evaluate sometimes came back
        // `no window for this folder` — which reads as a browser that was never
        // started rather than one that was not ready yet.
        run_browse(&[
            "wait",
            "--selector",
            "body",
            "--timeout",
            "60000",
            "--folder",
            ctx.folder,
        ])?;

        let page = Self {
            _browser: browser,
            producer,
            _dir: dir,
            blob_sha256,
        };
        arm()?;
        Ok(page)
    }

    /// Check the global invariant, then tear down.
    ///
    /// Called at the end of every cell rather than only the one about errors: a
    /// rejection is a failure wherever it lands, and checking costs nothing.
    fn finish(mut self) -> Res<()> {
        let raw = evaluate("JSON.stringify(window.__e2eRejections || [])")?;
        self.producer.interrupt();
        let found: Vec<String> = serde_json::from_str(&raw)
            .map_err(|error| format!("reading recorded rejections ({error}): {raw}"))?;
        if found.is_empty() {
            return Ok(());
        }
        Err(format!("unhandled rejection(s) reached the page: {found:?}").into())
    }
}

/// Install the in-page instrumentation, before anything can go wrong.
///
/// A free function rather than a method on [`Page`]: `evaluate` addresses
/// whichever window `agent-browse` finds for the folder, so nothing here is
/// scoped to a particular `Page` value and pretending otherwise would be a
/// small lie about what is being driven.
///
/// Two recorders, both armed ahead of the first action because what they catch
/// is transient:
///
/// - **Unhandled rejections**, the shape the crash overlay took. One leaves
///   nothing behind in the DOM to find afterwards.
/// - **The `reconnecting` crumb**, which appears and clears on its own. A poll
///   can miss it; an observer cannot.
///
/// It also replaces `showSaveFilePicker` with a memory sink that digests what
/// it is handed. That is a deliberate trade: the write to disk is the browser's,
/// and standing in for it buys a byte-exact assertion no other test in this repo
/// makes, while still exercising the whole stream — chunked reads, the zipper,
/// `pipeTo`, and the abort wiring. Setting `window.__e2eSaveAbort` makes the
/// stub reject the way a dismissed dialog does, which is otherwise a path no
/// cell could reach.
fn arm() -> Res<()> {
    // Retried rather than run once: the reload is asynchronous, so the first
    // evaluate can land in the outgoing document and be thrown away with it.
    // Arming twice is harmless — the script is idempotent.
    let deadline = Instant::now() + Duration::from_mins(1);
    loop {
        let attempt = evaluate(INSTRUMENT).and_then(|_| evaluate("String(!!window.__e2eArmed)"));
        match attempt {
            Ok(armed) if armed == "true" => return Ok(()),
            Ok(_) if Instant::now() >= deadline => {
                return Err("the page never kept the e2e instrumentation".into());
            }
            Err(error) if Instant::now() >= deadline => return Err(error),
            _ => std::thread::sleep(POLL),
        }
    }
}

/// Wait for the share listing to appear.
fn wait_for_listing() -> Res<()> {
    wait_for_true(
        "/blob\\.bin/.test(document.body.innerText)",
        CONNECT_TIMEOUT,
        "the share listing",
    )
}

/// The instrumentation injected into every page under test.
const INSTRUMENT: &str = r"
(() => {
  if (window.__e2eArmed) return 'already armed';
  window.__e2eArmed = true;
  window.__e2eRejections = [];
  addEventListener('unhandledrejection', (event) => {
    const reason = event.reason;
    window.__e2eRejections.push(String((reason && reason.message) || reason));
  });

  window.__e2eSawReconnecting = false;
  const note = () => {
    if (/reconnecting/i.test(document.body.innerText)) window.__e2eSawReconnecting = true;
  };
  new MutationObserver(note).observe(document.body, {
    subtree: true, childList: true, characterData: true,
  });
  note();

  // Total entries out of a ZIP end-of-central-directory record. A few hundred
  // small files is far below every Zip64 threshold, so the classic record
  // applies and the count is a plain little-endian uint16 at offset 10.
  const zipEntries = (bytes) => {
    for (let at = bytes.length - 22; at >= 0; at -= 1) {
      if (bytes[at] === 0x50 && bytes[at + 1] === 0x4b &&
          bytes[at + 2] === 0x05 && bytes[at + 3] === 0x06) {
        return bytes[at + 10] | (bytes[at + 11] << 8);
      }
    }
    return -1;
  };

  window.__e2eSaved = null;
  window.__e2eSaveAsks = 0;
  window.__e2eSaveAbort = false;
  window.showSaveFilePicker = (options) => {
    // Counted so a cell can tell `the dialog was dismissed` apart from `the
    // dialog never opened` — both leave `__e2eSaved` null.
    window.__e2eSaveAsks += 1;
    if (window.__e2eSaveAbort) {
      // Chrome's own wording, so the cell fails on the exact text a user saw.
      return Promise.reject(new DOMException(
        `Failed to execute 'showSaveFilePicker' on 'Window': The user aborted a request.`,
        'AbortError'));
    }
    return Promise.resolve({
      createWritable: () => {
        const parts = [];
        return Promise.resolve(new WritableStream({
          write(chunk) { parts.push(chunk); },
          async close() {
            const buffer = await new Blob(parts).arrayBuffer();
            const bytes = new Uint8Array(buffer);
            const digest = new Uint8Array(await crypto.subtle.digest('SHA-256', buffer));
            window.__e2eSaved = {
              name: options.suggestedName,
              bytes: bytes.length,
              sha256: [...digest].map((b) => b.toString(16).padStart(2, '0')).join(''),
              entries: zipEntries(bytes),
            };
          },
        }));
      },
    });
  };
  return 'armed';
})()
";

/// Serve `dir` and return the producer plus the ticket it printed.
fn serve(binary: &str, dir: &Path) -> Res<(Proc, String)> {
    let mut cmd = Command::new(binary);
    cmd.arg("serve").arg(dir).args(["--output", "json"]);
    let (producer, mut lines) = spawn_piped(cmd, "e2e producer")?;
    // `mount::announce` in json mode prints exactly `agent-share <ticket> .`.
    let Some(line) = lines.wait_for("agent-share ", Duration::from_mins(1)) else {
        return Err(format!(
            "the producer never printed a ticket; output was:\n{}",
            lines.transcript()
        )
        .into());
    };
    let ticket = line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| format!("no ticket in the producer's line: {line}"))?
        .to_owned();
    // `mesh.spawn_report(json)` keeps printing peer counts for the life of the
    // share, so the producer needs a reader for the same reason the dev server
    // does — see `Lines::drain_in_background`.
    lines.drain_in_background();
    Ok((producer, ticket))
}

// ---------------------------------------------------------------------------
// Driving the page
// ---------------------------------------------------------------------------

/// Poll a JS expression until it is truthy, or give up.
///
/// A timeout reports what the page was showing when it gave up. Without that a
/// failure is just a stopwatch: "connecting…", "Could not connect: <reason>"
/// and a blank body are three different bugs and read identically.
fn wait_for_true(expression: &str, timeout: Duration, what: &str) -> Res<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if evaluate(&format!("String(!!({expression}))"))? == "true" {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "timed out after {}s waiting for {what}; the page reads:\n{}",
                timeout.as_secs(),
                page_text()
            )
            .into());
        }
        std::thread::sleep(POLL);
    }
}

/// What the page currently says, trimmed to something readable in a log.
fn page_text() -> String {
    evaluate("(document.body.innerText || '').slice(0, 600)")
        .unwrap_or_else(|error| format!("<could not read the page: {error}>"))
}

/// Click a button by its label, waiting for it to exist and be enabled.
///
/// The match ignores case. Label casing is presentation here — the Button
/// component lowercases in CSS, and `innerText` reports what is *rendered*, so
/// an exact comparison would tie every cell to the current stylesheet and fail
/// on the next restyle. It would fail slowly, too: a miss is indistinguishable
/// from a button that has not appeared yet, so it costs the full minute below.
///
/// Enabled matters as much as present: the app holds back the actions that need
/// the peer while a revival runs, so "the button came back" is itself part of
/// what several cells assert.
fn click(label: &str) -> Res<()> {
    let expression = format!(
        "(()=>{{const want={}.toLowerCase();\
         const button=[...document.querySelectorAll('button')]\
         .find((b)=>(b.innerText||'').trim().toLowerCase()===want);\
         if(!button||button.disabled)return false;button.click();return true}})()",
        js_string(label)
    );
    wait_for_true(
        &expression,
        Duration::from_mins(1),
        &format!("an enabled `{label}` button"),
    )
}

/// What the save sink captured.
#[derive(Debug, serde::Deserialize)]
struct Saved {
    name: String,
    bytes: usize,
    sha256: String,
    /// ZIP entry count, or `-1` when the payload is not an archive.
    entries: i32,
}

/// Run a download to completion through the top-bar button.
fn download() -> Res<Saved> {
    click("Download")?;
    wait_for_true(
        "window.__e2eSaved !== null",
        ACTION_TIMEOUT,
        "the download to finish",
    )?;
    let raw = evaluate("JSON.stringify(window.__e2eSaved)")?;
    serde_json::from_str(&raw)
        .map_err(|error| format!("reading the saved file ({error}): {raw}").into())
}

// ---------------------------------------------------------------------------
// The cells
// ---------------------------------------------------------------------------

/// The floor: a browser dials a native producer and sees the tree.
fn cell_list(ctx: &Ctx<'_>) -> Res<()> {
    let page = Page::open(ctx, 3, "")?;
    wait_for_listing()?;
    let count =
        evaluate("String((document.body.innerText.match(/f[0-9]{3}\\.txt/g) || []).length)")?;
    if count.trim() != "3" {
        return Err(format!("expected 3 files in the tree, the page shows {count}").into());
    }
    page.finish()
}

/// Bytes arrive **identical to source**. Nothing else in this repo checks that.
///
/// The bench harness streams to `io::sink()` and reports a byte *count*, so a
/// transport that corrupted every byte would pass every one of its cells.
fn cell_download_single(ctx: &Ctx<'_>) -> Res<()> {
    // No extra files, so the whole share is one file and the app takes the
    // single-file path rather than wrapping it in an archive.
    let page = Page::open(ctx, 0, "")?;
    wait_for_listing()?;
    let file = download()?;
    if file.name != "blob.bin" {
        return Err(format!("expected the file itself, got `{}`", file.name).into());
    }
    if file.bytes != BLOB_LEN {
        return Err(format!("expected {BLOB_LEN} bytes, got {}", file.bytes).into());
    }
    if file.sha256 != page.blob_sha256 {
        return Err(format!(
            "the bytes that arrived are not the bytes on disk: got {}, want {}",
            file.sha256, page.blob_sha256
        )
        .into());
    }
    page.finish()
}

/// A share with more files than the producer's concurrent-stream ceiling.
///
/// `zipStream` used to build its entries with `.map()`, and a `ReadableStream`
/// on the default queueing strategy pulls as soon as it is constructed — so
/// every file opened a read before the zipper had asked for anything. Past 100
/// files (iroh's default `max_concurrent_bidi_streams`) that stalled on the
/// first tick. 300 is comfortably over.
fn cell_download_zip(ctx: &Ctx<'_>) -> Res<()> {
    let page = Page::open(ctx, 300, "")?;
    wait_for_listing()?;
    let archive = download()?;
    if Path::new(&archive.name).extension() != Some("zip".as_ref()) {
        return Err(format!("expected an archive, got `{}`", archive.name).into());
    }
    // Entry count, not byte count: the failure this guards leaves an archive
    // that is well-formed and short.
    if archive.entries != 301 {
        return Err(format!(
            "the archive holds {} entries, not the 301 files in the share",
            archive.entries
        )
        .into());
    }
    page.finish()
}

/// Dismissing the save dialog is a **no-op**, not a failure.
///
/// The bug: `downloadFiles` suppressed only the cancel *button* — it tested
/// `abort.signal.aborted`, which a dismissed picker never sets — so closing the
/// dialog left `Failed to execute 'showSaveFilePicker' on 'Window': The user
/// aborted a request.` in red under the top bar. The picker also opened after
/// the progress bar went up, so the dialog sat over a `downloading 0%` row with
/// a Cancel button next to it.
fn cell_download_dismissed(ctx: &Ctx<'_>) -> Res<()> {
    let page = Page::open(ctx, 0, "")?;
    wait_for_listing()?;

    evaluate("String(window.__e2eSaveAbort = true)")?;
    click("Download")?;
    wait_for_true(
        "window.__e2eSaveAsks > 0",
        ACTION_TIMEOUT,
        "the app to open the save dialog",
    )?;

    // The rejection is a microtask, but the render that would show it is not —
    // so this waits rather than sampling the frame before the one that draws.
    // There is nothing positive to poll for: on the fixed app the dismissal
    // changes nothing on screen, which is the whole point.
    std::thread::sleep(Duration::from_secs(2));
    let quiet = evaluate(
        "String(!/aborted|showSaveFilePicker|failed to execute/i\
         .test(document.body.innerText))",
    )?;
    if quiet.trim() != "true" {
        return Err(format!(
            "dismissing the dialog put an error on the page:\n{}",
            page_text()
        )
        .into());
    }
    if evaluate("String(window.__e2eSaved === null)")?.trim() != "true" {
        return Err("a dismissed dialog still saved a file".into());
    }
    // Not asserted here: that the progress bar stayed down *while* the dialog
    // was open. The stub rejects in a microtask, so there is no window in which
    // to observe it, and a check that cannot fail is not one. That half of the
    // fix is the Safari/Chrome manual row in `docs/testing.md`.

    // Back to normal is the actual claim, and it is the half a lone
    // no-error-text check would miss: a dismissal that left `transfer` set
    // wedges the button and every later download with it.
    evaluate("String(window.__e2eSaveAbort = false)")?;
    let file = download()?;
    if file.sha256 != page.blob_sha256 {
        return Err("the download after a dismissed dialog delivered the wrong bytes".into());
    }
    page.finish()
}

/// A connection killed under the app revives **unprompted**, and carries bytes.
///
/// The bug: a backgrounded tab lost its connection to QUIC's idle timeout — the
/// browser throttles timers past the keep-alive interval — and nothing reported
/// it. The tab looked perfectly connected and failed every action until it was
/// reloaded. Nothing is clicked here to provoke recovery, because a user who
/// never touches the tab must still come back to a working session.
fn cell_reconnect(ctx: &Ctx<'_>) -> Res<()> {
    let page = Page::open(ctx, 0, "?dev=true")?;
    wait_for_listing()?;
    click("Info")?;
    click("Kill connection")?;
    click("Close")?;

    wait_for_true(
        "window.__e2eSawReconnecting",
        ACTION_TIMEOUT,
        "the app to notice the connection died",
    )?;
    wait_for_true(
        "!/reconnecting/i.test(document.body.innerText) \
         && /blob\\.bin/.test(document.body.innerText)",
        ACTION_TIMEOUT,
        "the session to come back on its own",
    )?;

    // Back on screen is not the same as back in service. The bug's whole
    // signature was a page that looked connected and could not move a byte, so
    // this cell only passes if the revived session delivers the file.
    let file = download()?;
    if file.sha256 != page.blob_sha256 {
        return Err("the revived session delivered the wrong bytes".into());
    }
    page.finish()
}

/// A producer that goes away is reported **on the page**.
///
/// It used to arrive as an unhandled rejection in a crash overlay, naming an
/// internal stream operation that was not at fault. Two independent paths threw
/// there — the download, and the mesh departure during teardown — so this cell
/// asserts the readable failure *and* leans on `finish`'s rejection check,
/// which is the half that guards the teardown path.
fn cell_producer_gone(ctx: &Ctx<'_>) -> Res<()> {
    let mut page = Page::open(ctx, 3, "")?;
    wait_for_listing()?;
    page.producer.interrupt();
    wait_for_true(
        "/could not connect|lost|timed out|failed/i.test(document.body.innerText)",
        ACTION_TIMEOUT,
        "a readable failure on the page",
    )?;
    page.finish()
}

/// Each pinned data path carries a session end to end.
///
/// `webrtc` is the lane browsers depend on; `relay` is the fallback when ICE
/// fails, which is what a real Safari session did. Both are pinned by the URL,
/// so a session that quietly settled on the other path is a failure rather than
/// a footnote.
fn cell_transport_webrtc(ctx: &Ctx<'_>) -> Res<()> {
    cell_transport(ctx, "webrtc")
}

fn cell_transport_relay(ctx: &Ctx<'_>) -> Res<()> {
    cell_transport(ctx, "relay")
}

fn cell_transport(ctx: &Ctx<'_>, transport: &str) -> Res<()> {
    let page = Page::open(ctx, 3, &format!("?transport={transport}"))?;
    wait_for_listing()?;
    click("Info")?;
    wait_for_true(
        &format!("/transport {transport}\\b/.test(document.body.innerText)"),
        Duration::from_secs(30),
        &format!("the Info pane to report `transport {transport}`"),
    )?;
    page.finish()
}
