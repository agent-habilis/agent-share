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
use crate::bench::browser::{
    Browser, evaluate, evaluate_in, js_string, run_browse, start_dev_server,
};
use crate::bench::proc::{Proc, Res, TempDir, run_capture, spawn_piped};
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
    /// What this row needs beyond the suite-wide prerequisites, if anything.
    ///
    /// `Some(reason)` skips the row *with that reason* instead of failing it.
    /// A row whose dependency is genuinely absent has not found a defect, and
    /// reporting one would train people to ignore the report — but neither may
    /// it pass, which is the outcome a runner must never invent. Suite-wide
    /// needs stay in [`missing_prerequisite`]; this is for the one-row ones.
    precheck: Option<fn() -> Option<String>>,
}

/// The matrix, in one place so a skipped run still reports every row.
///
/// A prerequisite missing at the top means nothing can run, and the honest
/// output for that is a skip per row with a reason — not silence, not a pass.
const CELLS: &[Cell] = &[
    Cell {
        name: "web-list",
        run: cell_list,
        precheck: None,
    },
    Cell {
        name: "web-download-single",
        run: cell_download_single,
        precheck: None,
    },
    Cell {
        name: "web-download-zip",
        run: cell_download_zip,
        precheck: None,
    },
    Cell {
        name: "web-download-dismissed",
        run: cell_download_dismissed,
        precheck: None,
    },
    Cell {
        name: "web-reconnect",
        run: cell_reconnect,
        precheck: None,
    },
    Cell {
        name: "web-producer-gone",
        run: cell_producer_gone,
        precheck: None,
    },
    Cell {
        name: "web-live-update",
        run: cell_live_update,
        precheck: None,
    },
    Cell {
        name: "web-live-delete",
        run: cell_live_delete,
        precheck: None,
    },
    Cell {
        name: "web-seeder-propagation",
        run: cell_seeder_propagation,
        precheck: None,
    },
    Cell {
        name: "web-seeder-propagation-two-tabs",
        run: cell_seeder_propagation_two_tabs,
        precheck: None,
    },
    Cell {
        name: "web-transport-webrtc",
        run: cell_transport_webrtc,
        precheck: None,
    },
    Cell {
        name: "web-transport-relay",
        run: cell_transport_relay,
        precheck: None,
    },
    Cell {
        name: "web-password",
        run: cell_password,
        precheck: None,
    },
    Cell {
        name: "web-password-no-producer",
        run: cell_password_no_producer,
        precheck: None,
    },
    Cell {
        name: "password-native-live-right",
        run: cell_password_native_live_right,
        precheck: None,
    },
    Cell {
        name: "password-native-live-wrong",
        run: cell_password_native_live_wrong,
        precheck: None,
    },
    Cell {
        name: "password-native-dead-wrong",
        run: cell_password_native_dead_wrong,
        precheck: None,
    },
    Cell {
        name: "password-native-dead-right",
        run: cell_password_native_dead_right,
        precheck: None,
    },
    Cell {
        name: "password-native-absent",
        run: cell_password_native_absent,
        precheck: None,
    },
    Cell {
        name: "password-native-spurious",
        run: cell_password_native_spurious,
        precheck: None,
    },
    Cell {
        name: "password-native-mirror-reserve",
        run: cell_password_native_mirror_reserve,
        precheck: None,
    },
    Cell {
        name: "password-legacy-ticket",
        run: cell_password_legacy_ticket,
        precheck: None,
    },
    Cell {
        name: "password-web-dead-right",
        run: cell_password_web_dead_right,
        precheck: None,
    },
    Cell {
        name: "password-web-persist",
        run: cell_password_web_persist,
        precheck: None,
    },
    Cell {
        name: "password-node-cli",
        run: cell_password_node_cli,
        precheck: Some(node_datachannel_missing),
    },
    Cell {
        name: "password-web-producer",
        run: cell_password_web_producer,
        precheck: None,
    },
    Cell {
        name: "password-web-producer-snapshot",
        run: cell_password_web_producer_snapshot,
        precheck: None,
    },
    Cell {
        name: "webmcp-read",
        run: cell_webmcp_read,
        precheck: Some(webmcp_unavailable),
    },
    Cell {
        name: "webmcp-failures",
        run: cell_webmcp_failures,
        precheck: Some(webmcp_unavailable),
    },
    Cell {
        name: "webmcp-ui",
        run: cell_webmcp_ui,
        precheck: Some(webmcp_unavailable),
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
            if let Some(reason) = cell.precheck.and_then(|precheck| precheck()) {
                output::status("Skipping", cell.name);
                return Outcome {
                    cell: cell.name,
                    verdict: Verdict::Skip(reason),
                };
            }
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
            // Exact first, so a full name never picks up a longer sibling.
            if let Some(cell) = CELLS.iter().find(|cell| cell.name == name) {
                return Ok(vec![cell]);
            }
            // Then as a group prefix: `password` selects every `password-*`.
            // The matrix outgrew a list anyone would type in full, and the
            // groups are already spelled out in the names.
            let group: Vec<&'static Cell> = CELLS
                .iter()
                .filter(|cell| cell.name.starts_with(name))
                .collect();
            if !group.is_empty() {
                return Ok(group);
            }
            let known: Vec<&str> = CELLS.iter().map(|cell| cell.name).collect();
            Err(format!(
                "no such cell or group `{name}`; known cells are {}",
                known.join(", ")
            )
            .into())
        })
        .collect::<Res<Vec<Vec<&'static Cell>>>>()
        .map(|groups| {
            // Flatten, then drop repeats: `--cells password,password-native-absent`
            // names one cell twice, and running it twice would report it twice.
            let mut selected: Vec<&'static Cell> = Vec::new();
            for cell in groups.concat() {
                if !selected.iter().any(|seen| seen.name == cell.name) {
                    selected.push(cell);
                }
            }
            selected
        })
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

/// The fixture's digest, hashed here rather than by a child process.
///
/// This used to shell out to `shasum`/`sha256sum`, which cost a spawn per cell
/// and made "neither is on PATH" a reason to skip the entire suite. `sha2` was
/// already in the lockfile through `agent-share-proto`, so the dependency it
/// was avoiding did not exist.
fn sha256_file(path: &Path) -> Res<String> {
    use sha2::{Digest as _, Sha256};
    let bytes =
        std::fs::read(path).map_err(|error| format!("hashing {}: {error}", path.display()))?;
    let digest = Sha256::digest(&bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    Ok(out)
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
    /// The directory being served. Named rather than `_dir` so a cell can
    /// change the share while a tab is connected to it — which is the only way
    /// to test a live update at all.
    dir: TempDir,
    /// SHA-256 of the fixture's `blob.bin`, for the cells that compare bytes.
    blob_sha256: String,
}

impl Page {
    /// Serve `files + 1` fixture files and land a headless window on the share.
    fn open(ctx: &Ctx<'_>, files: usize, query: &str) -> Res<Self> {
        Self::open_protected(ctx, files, query, None)
    }

    /// As [`Self::open`], but the share is behind `password`.
    ///
    /// The window lands on the same URL — the ticket *is* the address, and its
    /// being postable without the password is the whole feature — so what
    /// differs is only what the page does on arrival.
    fn open_protected(
        ctx: &Ctx<'_>,
        files: usize,
        query: &str,
        password: Option<&str>,
    ) -> Res<Self> {
        let (dir, blob_sha256) = make_share(files)?;
        let (producer, ticket) = serve(ctx.binary, dir.path(), password)?;

        let target = format!("{}files/{ticket}{query}", ctx.url);
        run_browse(&["launch", "--headless", ctx.folder, &target])?;
        reap::track_browser(ctx.folder);
        let browser = Browser::new(ctx.folder.to_owned());

        // `scripts/dev.ts` serves the wasm straight from the crate's dist, so a
        // copy cached before the last `cargo task web-wasm` could survive a
        // plain reload. Clearing is
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
            dir,
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

// ---------------------------------------------------------------------------
// Driving the native binary
// ---------------------------------------------------------------------------

/// How long a native consumer may take before the cell calls it hung.
///
/// This is the *ceiling*, not the assertion. The rows that care about speed say
/// so with [`Attempt::faster_than`]; this only has to be generous enough for the
/// one row that must actually finish a dial — the right password against a dead
/// producer, which has to exhaust discovery before it can report the share
/// unreachable.
const NATIVE_TIMEOUT: Duration = Duration::from_mins(1);

/// Discovery deadline handed to the consumer under test.
///
/// The binary defaults to 90 s, which is right in the field and far too patient
/// for a suite that kills producers on purpose. Its own hidden knob, used the
/// way `consume.rs` documents it.
const DISCOVERY_SECS: &str = "5";

/// What running the consumer produced.
struct Attempt {
    /// Both streams joined: the error text lands on stderr, the progress on
    /// stdout, and every assertion here is about one string or the other.
    output: String,
    /// Whether the process exited 0.
    ok: bool,
    /// Wall-clock. The assertion, not a statistic, for the rows about a wrong
    /// password being ruled on locally.
    took: Duration,
}

impl Attempt {
    /// Fail unless the output contains `needle`.
    fn says(&self, needle: &str) -> Res<()> {
        if self.output.contains(needle) {
            return Ok(());
        }
        Err(format!(
            "expected the output to say `{needle}`, got:\n{}",
            self.output
        )
        .into())
    }

    /// Fail if the output contains `needle`.
    fn silent_about(&self, needle: &str) -> Res<()> {
        if self.output.contains(needle) {
            return Err(
                format!("the output should not mention `{needle}`:\n{}", self.output).into(),
            );
        }
        Ok(())
    }

    /// Fail unless it finished inside `limit`.
    ///
    /// The point of several rows: a wrong password used to be reported after a
    /// 96-second hang, and an answer that is merely *correct* is not the fix.
    fn faster_than(&self, limit: Duration) -> Res<()> {
        if self.took <= limit {
            return Ok(());
        }
        Err(format!(
            "took {:.1}s, which is past the {:.0}s this row exists to hold — \
             a local ruling became a network wait",
            self.took.as_secs_f64(),
            limit.as_secs_f64(),
        )
        .into())
    }
}

/// Run `agent-share mirror <ticket> <dest>` with an optional password.
///
/// `mirror` rather than the bare mount form: it needs no NFS, no mountpoint and
/// no privileges, exits on its own, and exercises the same `redeem_auth` gate
/// every consumer path goes through.
fn mirror_attempt(ctx: &Ctx<'_>, ticket: &str, password: Option<&str>) -> Res<(Attempt, TempDir)> {
    let dest = TempDir::new("e2e-mirror")?;
    let attempt = mirror_run(ctx, ticket, dest.path(), password)?;
    Ok((attempt, dest))
}

/// Mirror into a directory the caller owns, so a copy can be taken twice.
///
/// [`mirror_attempt`] makes its own `TempDir`, which is right for the rows that
/// only care whether one mirror succeeded. Catching a copy *up* needs the same
/// destination twice — the second run reads the sidecar and asks for the
/// difference.
fn mirror_into(ctx: &Ctx<'_>, ticket: &str, dest: &Path) -> Res<()> {
    let attempt = mirror_run(ctx, ticket, dest, None)?;
    if !attempt.ok {
        return Err(format!(
            "mirroring into {} failed:\n{}",
            dest.display(),
            attempt.output
        )
        .into());
    }
    Ok(())
}

/// The one place a `mirror` is spawned. Both callers differ only in who owns
/// the destination and whether a failure is fatal.
fn mirror_run(ctx: &Ctx<'_>, ticket: &str, dest: &Path, password: Option<&str>) -> Res<Attempt> {
    let mut cmd = Command::new(ctx.binary);
    cmd.arg("mirror").arg(ticket).arg(dest);
    if let Some(password) = password {
        cmd.args(["--password", password]);
    }
    // A dead producer means a long discovery retry, and that wait is precisely
    // what several rows measure.
    cmd.env("AGENT_SHARE_DISCOVERY_DEADLINE_SECS", DISCOVERY_SECS);
    let started = Instant::now();
    let captured = run_capture(cmd, "e2e mirror", NATIVE_TIMEOUT)?;
    Ok(Attempt {
        output: format!("{}{}", captured.stdout, captured.stderr),
        ok: captured.status.success(),
        took: started.elapsed(),
    })
}

// ---------------------------------------------------------------------------
// The password matrix
// ---------------------------------------------------------------------------

/// The password every row in this group uses.
const PASSWORD: &str = "e2e-hunter2";

/// Stand up a protected share and hand back its ticket.
fn protected_share(ctx: &Ctx<'_>) -> Res<(Proc, String, TempDir)> {
    let (dir, _sha) = make_share(1)?;
    let (producer, ticket) = serve(ctx.binary, dir.path(), Some(PASSWORD))?;
    Ok((producer, ticket, dir))
}

/// The ordinary path: the right password, a producer that is up, files arrive.
///
/// The row that would catch a check so strict it refuses everyone.
fn cell_password_native_live_right(ctx: &Ctx<'_>) -> Res<()> {
    let (mut producer, ticket, _dir) = protected_share(ctx)?;
    let (attempt, _dest) = mirror_attempt(ctx, &ticket, Some(PASSWORD))?;
    producer.interrupt();
    if !attempt.ok {
        return Err(format!(
            "the right password did not open the share:\n{}",
            attempt.output
        )
        .into());
    }
    attempt.says("Mirrored")
}

/// A wrong password, with the producer up. Named, and named locally.
///
/// The producer *could* answer this one — it would close the connection with
/// `CLOSE_UNAUTHORIZED` — so the timing is what says the ruling happened here
/// instead.
fn cell_password_native_live_wrong(ctx: &Ctx<'_>) -> Res<()> {
    let (mut producer, ticket, _dir) = protected_share(ctx)?;
    let (attempt, _dest) = mirror_attempt(ctx, &ticket, Some("not-the-password"))?;
    producer.interrupt();
    if attempt.ok {
        return Err("a wrong password opened the share".into());
    }
    attempt.says("does not open this share")?;
    attempt.faster_than(Duration::from_secs(10))
}

/// **The regression guard.** A wrong password, named with no producer at all.
///
/// This is the row the whole design turns on. A share is built to outlive its
/// producer, so a check that needs one to answer is a check that usually cannot
/// run — and before the ticket carried a mesh id this exact case spent 96
/// seconds on discovery and then blamed the network.
fn cell_password_native_dead_wrong(ctx: &Ctx<'_>) -> Res<()> {
    let (mut producer, ticket, _dir) = protected_share(ctx)?;
    producer.interrupt();
    let (attempt, _dest) = mirror_attempt(ctx, &ticket, Some("not-the-password"))?;
    if attempt.ok {
        return Err("a wrong password opened the share".into());
    }
    attempt.says("does not open this share")?;
    // Argon2id is ~100 ms; everything else here is process startup. Five
    // seconds is loose enough not to flake and far under any dial.
    attempt.faster_than(Duration::from_secs(5))
}

/// The right password against a producer that is gone must blame the *share*.
///
/// The false accusation this replaced: the consumer used to report a correct
/// password as refused whenever it could not reach anybody.
fn cell_password_native_dead_right(ctx: &Ctx<'_>) -> Res<()> {
    let (mut producer, ticket, _dir) = protected_share(ctx)?;
    producer.interrupt();
    let (attempt, _dest) = mirror_attempt(ctx, &ticket, Some(PASSWORD))?;
    if attempt.ok {
        return Err("a share with no producer and no seeder served bytes".into());
    }
    attempt.says("could not reach")?;
    attempt.silent_about("does not open this share")
}

/// A protected ticket with no password named at all is a usage error.
///
/// It must arrive before anything dials: there is nothing to try.
fn cell_password_native_absent(ctx: &Ctx<'_>) -> Res<()> {
    let (mut producer, ticket, _dir) = protected_share(ctx)?;
    producer.interrupt();
    let (attempt, _dest) = mirror_attempt(ctx, &ticket, None)?;
    if attempt.ok {
        return Err("a protected share opened with no password".into());
    }
    attempt.says("password-protected")?;
    attempt.faster_than(Duration::from_secs(5))
}

/// A password offered to an *unprotected* ticket names the ticket, not the
/// password.
///
/// Almost always the wrong link rather than the wrong password, and silently
/// ignoring it would open the wrong share and look like success.
fn cell_password_native_spurious(ctx: &Ctx<'_>) -> Res<()> {
    let (dir, _sha) = make_share(1)?;
    let (mut producer, ticket) = serve(ctx.binary, dir.path(), None)?;
    producer.interrupt();
    let (attempt, _dest) = mirror_attempt(ctx, &ticket, Some(PASSWORD))?;
    if attempt.ok {
        return Err("a password was accepted for an unprotected share".into());
    }
    attempt.says("not password-protected")?;
    attempt.faster_than(Duration::from_secs(5))
}

/// A mirror of a protected share, re-served without the password.
///
/// The documented degradation: `fofoca` gates every mesh derivation behind the
/// stretched password key, so such a copy cannot join the share's mesh. It must
/// still *serve* — the token in its sidecar opens the mount protocol — because
/// serving nothing would be the worse trade. Asserted from the outside: a fresh
/// consumer with the password reads the tree out of the re-server.
fn cell_password_native_mirror_reserve(ctx: &Ctx<'_>) -> Res<()> {
    let (mut origin, ticket, _dir) = protected_share(ctx)?;
    let (copied, dest) = mirror_attempt(ctx, &ticket, Some(PASSWORD))?;
    if !copied.ok {
        return Err(format!("the mirror failed:\n{}", copied.output).into());
    }
    origin.interrupt();

    // No `--password`: the copy has only what the mirror left beside it.
    let (mut reserver, reserved_ticket) = serve(ctx.binary, dest.path(), None)?;
    let (read_back, _dest2) = mirror_attempt(ctx, &reserved_ticket, Some(PASSWORD))?;
    reserver.interrupt();
    if !read_back.ok {
        return Err(format!(
            "a re-served protected mirror did not serve its bytes:\n{}",
            read_back.output
        )
        .into());
    }
    read_back.says("Mirrored")
}

/// A protected ticket that carries **no mesh id**, as an older producer minted.
///
/// The compatibility row. Such a ticket has no verifier to rule against, so the
/// local check cannot run and the consumer falls back to what it always did:
/// present the token and let the producer refuse it. That fallback is the only
/// path where the origin still has to be alive, which is why this row keeps one
/// up — and why the wrong-password half asserts nothing about elapsed time.
///
/// Built by stripping the id off a real ticket rather than by mocking one, so
/// the row breaks if the field ever stops being optional.
fn cell_password_legacy_ticket(ctx: &Ctx<'_>) -> Res<()> {
    let (mut producer, ticket, _dir) = protected_share(ctx)?;
    let legacy = strip_mesh_id(&ticket)?;

    let (wrong, _a) = mirror_attempt(ctx, &legacy, Some("not-the-password"))?;
    if wrong.ok {
        return Err("a wrong password opened a legacy ticket".into());
    }
    // The producer's refusal, reworded by the consumer — same words as the
    // local ruling, because the user does not care which end decided.
    wrong.says("does not open this share")?;

    let (right, _b) = mirror_attempt(ctx, &legacy, Some(PASSWORD))?;
    producer.interrupt();
    if !right.ok {
        return Err(format!(
            "a legacy ticket refused the right password:\n{}",
            right.output
        )
        .into());
    }
    right.says("Mirrored")
}

/// Re-encode `ticket` with its mesh id removed — a ticket as an older producer
/// would have minted it.
///
/// Through the real codec, not a copy of the format written here: a second
/// implementation in the harness would drift from the one under test, and the
/// drift would show up as a passing row.
fn strip_mesh_id(ticket: &str) -> Res<String> {
    let mut decoded = agent_share_proto::ticket::MountTicket::decode(ticket)
        .map_err(|error| format!("decoding the ticket to strip its mesh id: {error}"))?;
    decoded.mesh_id = None;
    Ok(decoded.encode())
}

/// The browser twin of the false-accusation guard.
///
/// Right password, producer already dead. The tab cannot reach anybody, so it
/// keeps trying — that is the app's posture and not a fault. What it must never
/// do is blame the password, which is exactly what it did before the ruling
/// moved off the network: the seeder wait timed out and the gate came back
/// saying the password was refused, about a password that was correct.
///
/// A negative assertion held over time, because the failure it guards is
/// something *appearing* rather than something missing. The window is past the
/// 30 s seeder-card deadline where the old accusation was minted.
fn cell_password_web_dead_right(ctx: &Ctx<'_>) -> Res<()> {
    let (dir, _sha) = make_share(3)?;
    let (mut producer, ticket) = serve(ctx.binary, dir.path(), Some(PASSWORD))?;
    producer.interrupt();

    // Bound, not discarded: dropping the guard closes the window.
    let _browser = open_window(ctx, &format!("{}files/{ticket}", ctx.url))?;
    wait_for_true(
        "/Password required/.test(document.body.innerText)",
        CONNECT_TIMEOUT,
        "the password gate",
    )?;
    type_password(PASSWORD)?;
    click("Open share")?;

    let deadline = Instant::now() + Duration::from_secs(45);
    while Instant::now() < deadline {
        if evaluate("String(/does not open this share/.test(document.body.innerText))")?.trim()
            == "true"
        {
            return Err(
                "the page blamed a correct password for a share it simply could not reach".into(),
            );
        }
        std::thread::sleep(POLL);
    }
    Ok(())
}

/// A password typed once is not asked for again in the same tab.
///
/// `sessionStorage`, keyed by a digest of the ticket. Two things follow and both
/// are asserted: a reload does not re-prompt, and neither does moving between
/// the share's two views. A tab that forgot would also re-pay ~100 ms of
/// Argon2id every time, but the reason this row exists is the re-prompt.
fn cell_password_web_persist(ctx: &Ctx<'_>) -> Res<()> {
    let page = Page::open_protected(ctx, 3, "", Some(PASSWORD))?;
    wait_for_true(
        "/Password required/.test(document.body.innerText)",
        CONNECT_TIMEOUT,
        "the password gate",
    )?;
    type_password(PASSWORD)?;
    click("Open share")?;
    wait_for_listing()?;

    run_browse(&["cdp", "Page.reload", "{}", "--folder", ctx.folder])?;
    run_browse(&[
        "wait",
        "--selector",
        "body",
        "--timeout",
        "60000",
        "--folder",
        ctx.folder,
    ])?;
    // Straight back to the listing. If the gate returns, the tab forgot.
    wait_for_listing()?;
    if evaluate("String(/Password required/.test(document.body.innerText))")?.trim() == "true" {
        return Err("a reload re-prompted for a password the tab had already taken".into());
    }

    // And the other view of the same share, which is a route change rather than
    // a reload — a different path through the session, same stored password.
    click("Info")?;
    if evaluate("String(/Password required/.test(document.body.innerText))")?.trim() == "true" {
        return Err("switching to the info view re-prompted for the password".into());
    }
    page.finish()
}

/// `npx agent-share <ticket> --password` — the third client.
///
/// The node CLI links the same wasm as the browser but drives it from a
/// process, so it is the one platform where a break would show up in neither
/// the native tests nor the browser cells.
fn cell_password_node_cli(ctx: &Ctx<'_>) -> Res<()> {
    let root = util::repo_root();
    let cli = root.join("node/src/cli.js");
    let (dir, _sha) = make_share(1)?;
    let (mut producer, ticket) = serve(ctx.binary, dir.path(), Some(PASSWORD))?;

    let dest = TempDir::new("e2e-node")?;
    let mut cmd = Command::new("node");
    cmd.arg(&cli)
        .arg(&ticket)
        .arg(dest.path())
        .args(["--password", PASSWORD]);
    let ok = run_capture(cmd, "e2e node consumer", NATIVE_TIMEOUT)?;

    let wrong_dest = TempDir::new("e2e-node-wrong")?;
    let mut wrong_cmd = Command::new("node");
    wrong_cmd
        .arg(&cli)
        .arg(&ticket)
        .arg(wrong_dest.path())
        .args(["--password", "not-the-password"]);
    let wrong = run_capture(wrong_cmd, "e2e node consumer (wrong)", NATIVE_TIMEOUT)?;
    producer.interrupt();

    if !ok.status.success() {
        return Err(format!(
            "the node CLI refused the right password:\n{}{}",
            ok.stdout, ok.stderr
        )
        .into());
    }
    if wrong.status.success() {
        return Err("the node CLI accepted a wrong password".into());
    }
    let said = format!("{}{}", wrong.stdout, wrong.stderr);
    if !said.contains("password") {
        return Err(format!("the node CLI did not name the password:\n{said}").into());
    }
    Ok(())
}

/// Why the node row cannot run here, if it cannot.
///
/// `node-datachannel` is an *optional* dependency of the node package (Node has
/// no built-in `RTCPeerConnection`), so a clean checkout does not have it and
/// the row would fail for a reason that is not a defect.
fn node_datachannel_missing() -> Option<String> {
    // Whether it *loads*, not whether the directory is there. The package can
    // be installed while its native addon is not built for this Node — which is
    // the state a bun install leaves, and it fails at import rather than at
    // resolution, so a path check reports it as present and the row then fails
    // for a reason that is not a defect.
    let node_dir = util::repo_root().join("node");
    let loaded = Command::new("node")
        .current_dir(&node_dir)
        .args(["-e", "import('node-datachannel/polyfill')"])
        .output();
    match loaded {
        Ok(output) if output.status.success() => None,
        Ok(_) => Some(
            "node-datachannel does not load — Node has no built-in RTCPeerConnection, and \
             the native addon is missing or not built for this Node (cd node && npm install)"
                .to_owned(),
        ),
        Err(error) => Some(format!("node is not runnable: {error}")),
    }
}

/// **A browser tab creates a protected share; the native CLI opens it.**
///
/// The direction nothing else covers. Every other row has a native producer, so
/// the browser's `set_password` and the mesh id it mints were never checked
/// against what the native side derives from the same ticket — and they have to
/// agree byte for byte or the two ends land on different meshes.
///
/// Producing in the app needs a picker, which needs a user gesture and cannot
/// be driven headlessly, which is why this row did not exist. `/lab` stands the
/// same `startProducer` up from a source that needs no gesture.
fn cell_password_web_producer(ctx: &Ctx<'_>) -> Res<()> {
    // Stated, not inferred: a Chrome without OPFS must fail this row with the
    // reason, never pass it. The `precheck` hook cannot help — it runs before
    // any browser exists.
    web_producer_over(ctx, "opfs", |_| {
        let opfs = evaluate("String(typeof navigator.storage?.getDirectory === 'function')")?;
        if opfs.trim() != "true" {
            return Err(
                "this browser has no origin private file system, so a tab cannot \
                        produce a share without the directory picker"
                    .into(),
            );
        }
        Ok(())
    })
}

/// **The same, from `File`s instead of handles — the Safari and Firefox path.**
///
/// Those browsers have no directory picker, so they share through
/// `<input type="file">` and the producer serves `File`s: same lazy ranged
/// reads, but pinned to what was picked. The branch is a different arm of
/// `FileSource` all the way down, and nothing else exercises it.
///
/// Runs in headless Chrome like every other row, so it proves the *branch*, not
/// Safari. It needs no OPFS, which is why it carries no precheck of its own.
fn cell_password_web_producer_snapshot(ctx: &Ctx<'_>) -> Res<()> {
    web_producer_over(ctx, "snapshot", |_| Ok(()))
}

/// Drive `/lab`'s share panel in `mode`, then open what it serves from the CLI.
fn web_producer_over(ctx: &Ctx<'_>, mode: &str, ready: impl Fn(&Browser) -> Res<()>) -> Res<()> {
    let browser = open_window(ctx, &format!("{}lab", ctx.url))?;
    ready(&browser)?;

    let started = format!(
        "(() => {{ \
           document.getElementById('share-files').value = '2'; \
           document.getElementById('share-mode').value = {}; \
           document.getElementById('share-password').value = {}; \
           document.getElementById('share-start').click(); \
           return true }})()",
        js_string(mode),
        js_string(PASSWORD)
    );
    wait_for_true(&started, Duration::from_secs(30), "the lab share panel")?;

    wait_for_true(
        "document.getElementById('share-ticket').value",
        CONNECT_TIMEOUT,
        "the tab to mint a ticket for its own share",
    )?;
    // `String(…)` explicitly: `evaluate` reads `/result/value` as a JSON string
    // and yields "" for anything else, so an unwrapped `.value` can come back
    // empty even though the wait above just saw it populated.
    let ticket = evaluate("String(document.getElementById('share-ticket').value)")?
        .trim()
        .to_owned();
    if ticket.is_empty() {
        let log = evaluate("String(document.getElementById('share-log').textContent)")
            .unwrap_or_default();
        return Err(
            format!("the lab reported a ticket that was empty; its log says:\n{log}").into(),
        );
    }

    // A wrong password is refused, and refused locally — the verifier in the
    // mesh id the *browser* minted is what the native side checks against.
    let (wrong, _a) = mirror_attempt(ctx, &ticket, Some("not-the-password"))?;
    if wrong.ok {
        return Err("a wrong password opened a browser-produced share".into());
    }
    wrong.says("does not open this share")?;
    wrong.faster_than(Duration::from_secs(10))?;

    // And the right one reads the tree the tab is serving.
    let (right, dest) = mirror_attempt(ctx, &ticket, Some(PASSWORD))?;
    if !right.ok {
        return Err(format!(
            "the native CLI could not open a browser-produced share:\n{}",
            right.output
        )
        .into());
    }
    // The two shapes the fixture carries on purpose. A nested directory has to
    // survive the manifest, and a zero-byte file has a slot with nothing to
    // read — the case that found the browser producer dropping `OP_HASH`
    // instead of answering "cannot vouch".
    for expected in ["nested/deep.txt", "empty.txt", "blob.bin"] {
        if !dest.path().join(expected).exists() {
            return Err(format!("`{expected}` did not arrive from the browser share").into());
        }
    }

    // Stop the share so the lab removes any OPFS directory rather than leaving
    // it for the next run.
    let _ = evaluate("(() => { document.getElementById('share-stop').click(); return true })()");
    drop(browser);
    Ok(())
}

/// A second window's folder key.
///
/// `agent-browse` gives one window per folder and keys its profile by that
/// path, so a second peer needs a second real directory — and the separate
/// profile is the point, not a side effect: one shared `IndexedDB` would let
/// the "fresh" consumer serve itself from its own chunk store and pass a
/// propagation test while proving nothing.
///
/// `target/` rather than a source directory, because the bare `evaluate` drives
/// whatever window matches the *current* directory: pick somewhere a person
/// might plausibly run `cargo task e2e` from and the two windows trade places.
fn second_folder() -> String {
    util::repo_root().join("target").display().to_string()
}

/// [`wait_for_true`], against the window keyed to `folder`.
fn wait_for_true_in(folder: &str, expression: &str, timeout: Duration, what: &str) -> Res<()> {
    wait_for_true_at(Some(folder), expression, timeout, what)
}

/// Poll `expression` until it is truthy, in the window `folder` names — or in
/// the one the current directory resolves to when it is `None`.
///
/// The shape `bench::browser`'s `evaluate_at` already uses: two public names,
/// one deadline loop, so a change to the poll interval or the failure message
/// cannot reach one caller and miss the other.
fn wait_for_true_at(
    folder: Option<&str>,
    expression: &str,
    timeout: Duration,
    what: &str,
) -> Res<()> {
    let deadline = Instant::now() + timeout;
    let wrapped = format!("String(!!({expression}))");
    loop {
        let seen = match folder {
            Some(folder) => evaluate_in(folder, &wrapped)?,
            None => evaluate(&wrapped)?,
        };
        if seen == "true" {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "timed out after {}s waiting for {what}; the page reads:\n{}",
                timeout.as_secs(),
                match folder {
                    Some(folder) => page_text_in(folder),
                    None => page_text(),
                }
            )
            .into());
        }
        std::thread::sleep(POLL);
    }
}

/// [`open_window`], against an explicit folder, for a cell that runs two peers.
fn open_window_in(folder: &str, target: &str) -> Res<Browser> {
    run_browse(&["launch", "--headless", folder, target])?;
    reap::track_browser(folder);
    let browser = Browser::new(folder.to_owned());
    run_browse(&[
        "wait",
        "--selector",
        "body",
        "--timeout",
        "60000",
        "--folder",
        folder,
    ])?;
    Ok(browser)
}

/// Launch a headless window on `target` and wait for it to be drivable.
///
/// The part of [`Page::open`] that does not need a producer of its own, for the
/// rows that stand one up themselves — or kill it first.
fn open_window(ctx: &Ctx<'_>, target: &str) -> Res<Browser> {
    open_window_in(ctx.folder, target)
}

/// Serve `dir` and return the producer plus the ticket it printed.
///
/// `password` protects the share. The printed line is unchanged either way —
/// json mode is read by machines and stays one runnable command — so a
/// protected share is recognised from the *ticket*, not from the output.
fn serve(binary: &str, dir: &Path, password: Option<&str>) -> Res<(Proc, String)> {
    let mut cmd = Command::new(binary);
    cmd.arg("serve").arg(dir).args(["--output", "json"]);
    if let Some(password) = password {
        cmd.args(["--password", password]);
    }
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
    wait_for_true_at(None, expression, timeout, what)
}

/// What the page currently says, trimmed to something readable in a log.
fn page_text() -> String {
    read_page(evaluate(PAGE_TEXT))
}

/// [`page_text`], from the window keyed to `folder`.
fn page_text_in(folder: &str) -> String {
    read_page(evaluate_in(folder, PAGE_TEXT))
}

/// Truncated on purpose: a failure message carrying a whole page body is a
/// failure message nobody reads.
const PAGE_TEXT: &str = "(document.body.innerText || '').slice(0, 600)";

fn read_page(result: Res<String>) -> String {
    result.unwrap_or_else(|error| format!("<could not read the page: {error}>"))
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
// WebMCP
// ---------------------------------------------------------------------------

/// Every `f{index:03}.txt` the fixture writes — see [`make_share`]. Spelled as
/// the string itself so it cannot drift from what is actually served.
const FIXTURE_FILE_LEN: usize = "file 0 contents\n".len();

/// Chrome below this cannot publish tools at all, so the rows would fail for a
/// reason that is not a defect. 150 is the floor `docs/webmcp.md` names.
const WEBMCP_MIN_CHROME: u32 = 150;

/// Run a row's assertions in the page, and turn its report into a verdict.
///
/// `call` goes out through `executeTool` rather than reaching into the page's
/// modules, which is the entire point of covering this here: the unit tests
/// already exercise the tool bodies, and what they structurally cannot reach is
/// the browser's own dispatch — argument serialization, the result coming back
/// as a JSON *string*, and the fact that nothing validates `inputSchema` on the
/// way in.
///
/// Every miss is reported, not just the first. A row that stopped at the first
/// bad answer would need a run per assertion to see the shape of a breakage,
/// and these rows cost a relay handshake each.
fn run_webmcp(body: &str) -> Res<()> {
    let script = format!(
        r"(async () => {{
          if (!document.modelContext) {{
            return JSON.stringify(['this browser exposes no document.modelContext']);
          }}
          const tools = Object.fromEntries(
            (await document.modelContext.getTools()).map((tool) => [tool.name, tool]),
          );
          const bad = [];
          const call = async (name, args) => {{
            if (!tools[name]) {{
              bad.push('the page never published ' + name);
              return {{}};
            }}
            const raw = await document.modelContext.executeTool(
              tools[name], JSON.stringify(args ?? {{}}),
            );
            return typeof raw === 'string' ? JSON.parse(raw) : raw;
          }};
          const check = (label, got, want) => {{
            const g = JSON.stringify(got);
            const w = JSON.stringify(want);
            if (g !== w) bad.push(label + ': got ' + g + ', wanted ' + w);
          }};
          {body}
          return JSON.stringify(bad);
        }})()"
    );
    let raw = evaluate(&script)?;
    let failures: Vec<String> = serde_json::from_str(&raw)
        .map_err(|error| format!("reading the WebMCP report ({error}): {raw}"))?;
    if failures.is_empty() {
        return Ok(());
    }
    Err(failures.join("; ").into())
}

/// Why the `WebMCP` rows cannot run here, if they cannot.
///
/// A precheck runs before any window exists, so it cannot ask the page itself
/// and settles for the version instead. That is the weaker of the two things
/// `docs/webmcp.md` requires — the browser must also expose the property on the
/// origin under test — so a row whose browser is new enough but still publishes
/// nothing fails on the guard at the top of [`run_webmcp`], naming what it
/// found. Reported as a skip only where the version alone already settles it.
fn webmcp_unavailable() -> Option<String> {
    let Some(reported) = crate::bench::chrome_version() else {
        return Some("agent-browse reports no Chrome version".to_owned());
    };
    let major = reported
        .split('.')
        .next()
        .and_then(|major| major.parse::<u32>().ok());
    let Some(major) = major else {
        return Some(format!(
            "agent-browse reports an unreadable Chrome version: {reported}"
        ));
    };
    if major < WEBMCP_MIN_CHROME {
        return Some(format!(
            "Chrome {major} has no WebMCP — {WEBMCP_MIN_CHROME}+ is needed \
             (agent-browse chrome install --execute)"
        ));
    }
    None
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

/// **A file added while a tab is connected reaches it, with no reload.**
///
/// The path the whole watch stack exists for, and nothing covered it: every
/// other cell writes its fixture before the producer starts, so a producer that
/// never published a change would have passed all of them.
fn cell_live_update(ctx: &Ctx<'_>) -> Res<()> {
    let page = Page::open(ctx, 3, "")?;
    wait_for_listing()?;

    std::fs::write(page.dir.path().join("added-live.txt"), "added while live\n")
        .map_err(|error| format!("adding a file to the served share: {error}"))?;

    wait_for_true(
        "/added-live\\.txt/.test(document.body.innerText)",
        CONNECT_TIMEOUT,
        "a file added while connected to reach the tab",
    )?;
    page.finish()
}

/// **And a file removed while connected leaves it.**
///
/// The other direction, which goes through the tombstone path — a removed slot
/// is kept so indices never shift, so "gone from the tree" and "gone from the
/// manifest" are deliberately not the same thing.
fn cell_live_delete(ctx: &Ctx<'_>) -> Res<()> {
    let page = Page::open(ctx, 3, "")?;
    wait_for_listing()?;
    wait_for_true(
        "/f001\\.txt/.test(document.body.innerText)",
        CONNECT_TIMEOUT,
        "the fixture file the test is about to remove",
    )?;

    std::fs::remove_file(page.dir.path().join("f001.txt"))
        .map_err(|error| format!("removing a file from the served share: {error}"))?;

    wait_for_true(
        "String(!/f001\\.txt/.test(document.body.innerText))",
        CONNECT_TIMEOUT,
        "a file removed while connected to leave the tab",
    )?;
    page.finish()
}

/// **A change reaches a peer that never met the producer.**
///
/// The other half of the invariant. Everything else asserts that only the
/// creator can *author* a version; this asserts that anybody may *carry* one —
/// which is what makes a share outlive the process that made it.
///
/// The origin publishes a second version, a copy catches up to it, and only then
/// does the origin die. A tab opened afterwards holds the **original** ticket,
/// whose address points at a process that is gone, so the tree it renders can
/// only have come from the copy — and the copy holds no signing key
/// (`mount::produce::authorship_for`), so the signature on it is still the
/// creator's or the tab would have refused it.
fn cell_seeder_propagation(ctx: &Ctx<'_>) -> Res<()> {
    let (dir, _sha) = make_share(3)?;
    let (mut origin, ticket) = serve(ctx.binary, dir.path(), None)?;

    // A copy taken at v1, before the change exists.
    let copy = TempDir::new("e2e-seeder")?;
    mirror_into(ctx, &ticket, copy.path())?;
    if copy.path().join("added-live.txt").exists() {
        return Err("the fixture already had the file this row adds".into());
    }

    // v2.
    std::fs::write(dir.path().join("added-live.txt"), "added while live\n")
        .map_err(|error| format!("adding a file to the served share: {error}"))?;
    // The copy catches up, re-serving the creator's signature verbatim.
    //
    // Retried rather than slept on. The producer debounces a rescan for 300 ms
    // and publishes once it settles, so the honest wait is "until the copy has
    // it" — and a mirror's own connect usually outlasts the debounce, so this
    // costs one attempt and no fixed delay.
    let deadline = Instant::now() + NATIVE_TIMEOUT;
    loop {
        mirror_into(ctx, &ticket, copy.path())?;
        if copy.path().join("added-live.txt").exists() {
            break;
        }
        if Instant::now() >= deadline {
            return Err("the copy never caught up to the producer's change".into());
        }
        std::thread::sleep(POLL);
    }

    // From here the copy is the only source of the share.
    let (mut seeder, _seeder_ticket) = serve(ctx.binary, copy.path(), None)?;
    origin.interrupt();

    let _browser = open_window(ctx, &format!("{}files/{ticket}", ctx.url))?;
    let found = wait_for_true(
        "/added-live\\.txt/.test(document.body.innerText)",
        CONNECT_TIMEOUT,
        "a change made before the origin died to reach a tab served by a seeder",
    );
    seeder.interrupt();
    found
}

/// **A browser tab carries a change to another browser tab.**
///
/// The shape the requirement is actually about, and the one
/// [`cell_seeder_propagation`] cannot reach: there the seeder is a native
/// process, here it is a tab. Two windows, so two Chrome profiles and two
/// `IndexedDB`s — a shared one would let the second tab serve itself and pass
/// while proving nothing.
///
/// Tab A reads the share, so it holds bytes and can answer for them. The origin
/// then publishes a change and dies. Tab B — a different profile, holding
/// nothing — opens the **original** ticket and must still see the change, which
/// by then exists nowhere but in tab A.
///
/// Default transport on the consumer, not a pinned one. Reaching a *browser*
/// seeder means forming a data channel to it, and only the dynamic lane both
/// falls back to a seeder and can negotiate one: `webrtc` pins the lane and has
/// no seeder fallback at all, while `relay` has the fallback but no way to dial
/// a peer that lives in a tab.
fn cell_seeder_propagation_two_tabs(ctx: &Ctx<'_>) -> Res<()> {
    let (dir, _sha) = make_share(3)?;
    let (mut origin, ticket) = serve(ctx.binary, dir.path(), None)?;

    // Tab A takes the whole share, which is what arms it as a seeder: `Seed`
    // fetches every slot and republishes what landed. `Seeding` is the label the
    // button takes once it holds everything, so waiting for it is waiting for
    // the bytes rather than for the click.
    let _a = open_window(ctx, &format!("{}files/{ticket}", ctx.url))?;
    wait_for_listing()?;
    click("Seed")?;
    wait_for_true(
        "[...document.querySelectorAll('button')]\
         .some((b)=>(b.innerText||'').trim().toLowerCase()==='seeding')",
        ACTION_TIMEOUT,
        "tab A to hold the whole share, so it has something to seed",
    )?;

    // The origin publishes a change. Tab A verifies it and re-arms as a seeder
    // for the new version; without that the change would stop here.
    std::fs::write(dir.path().join("added-live.txt"), "added while live\n")
        .map_err(|error| format!("adding a file to the served share: {error}"))?;
    wait_for_true(
        "/added-live\\.txt/.test(document.body.innerText)",
        CONNECT_TIMEOUT,
        "tab A to take the change before it becomes the only copy of it",
    )?;

    // From here the change exists only in tab A. `interrupt` waits for the
    // process to actually go, so there is nothing left to sleep for.
    origin.interrupt();

    let folder_b = second_folder();
    let _b = open_window_in(&folder_b, &format!("{}files/{ticket}", ctx.url))?;
    wait_for_true_in(
        &folder_b,
        "/added-live\\.txt/.test(document.body.innerText)",
        ACTION_TIMEOUT,
        "a second tab to receive a change that only another tab still holds",
    )
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

    // The Info panel stays open across this wait, and that is load-bearing.
    // `reconnecting` is rendered in exactly one place — `TechInfo`'s status —
    // because the breadcrumb deliberately never says it ("redialing is the
    // app's permanent background posture … naming it in the chrome would label
    // the normal state of the world", `web/src/pages/files/index.tsx`). This
    // cell used to close the panel first and then wait for a word only the
    // panel renders.
    wait_for_true(
        "window.__e2eSawReconnecting",
        ACTION_TIMEOUT,
        "the app to notice the connection died",
    )?;
    // Still inside the Info panel: it is the only surface that renders the
    // label, so "no longer reconnecting" is only a real assertion while it is
    // open. Closing first made this vacuous — the word is absent from a page
    // that never shows it, and the cell then tried to download over a session
    // that was still redialling.
    wait_for_true(
        "!/reconnecting/i.test(document.body.innerText)",
        ACTION_TIMEOUT,
        "the session to come back on its own",
    )?;
    click("Close")?;
    wait_for_true(
        "/blob\\.bin/.test(document.body.innerText)",
        ACTION_TIMEOUT,
        "the file listing after the session recovered",
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

/// A producer that goes away does not take the page with it.
///
/// What this asserts changed with `25d20897` (*let seeders keep a share alive
/// after its producer dies*). Before it, a dead producer was a failure and the
/// cell waited for one to be *reported*; the bug it was written for was that
/// the report arrived as an unhandled rejection in a crash overlay, naming an
/// internal stream operation that was not at fault.
///
/// Now the tab holds the manifest and the listing survives, so waiting for a
/// failure waits forever. The property worth holding is the one that bug was
/// really about: nothing crashes, and when an action genuinely cannot be served
/// — no producer, no seeder — it *says so* instead of throwing into the void.
/// `finish`'s rejection check is still the half that guards the teardown path.
fn cell_producer_gone(ctx: &Ctx<'_>) -> Res<()> {
    let mut page = Page::open(ctx, 3, "")?;
    wait_for_listing()?;
    page.producer.interrupt();

    // The listing outlives the producer. Asserted after a pause long enough for
    // the connection to actually die, so this cannot pass on staleness alone.
    std::thread::sleep(Duration::from_secs(5));
    if evaluate("String(/blob\\.bin/.test(document.body.innerText))")?.trim() != "true" {
        return Err("the listing did not survive the producer".into());
    }

    // But the bytes are gone, and the page says so the way a UI should: by
    // refusing the action rather than by accepting it and throwing. Asserted as
    // "no *enabled* Download button", which covers both disabling it and
    // removing it — the cell is about the user not being led into a failure,
    // not about which of the two the app picks.
    wait_for_true(
        "[...document.querySelectorAll('button')] \
         .every((b) => (b.innerText || '').trim().toLowerCase() !== 'download' || b.disabled)",
        ACTION_TIMEOUT,
        "the download action to stop being offered once nobody can serve it",
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

/// Type `password` into the gate's field, the way a person would.
///
/// Sets the value and dispatches `input`, because the field is uncontrolled and
/// the component reads it from that event — assigning `.value` alone updates the
/// pixels and tells the app nothing.
fn type_password(password: &str) -> Res<()> {
    let expression = format!(
        "(()=>{{const field=document.querySelector('input[type=password]');\
         if(!field)return false;field.value={};\
         field.dispatchEvent(new Event('input',{{bubbles:true}}));return true}})()",
        js_string(password)
    );
    wait_for_true(&expression, Duration::from_secs(30), "the password field")
}

/// **The browser half of password-protected shares.**
///
/// One window, one protected ticket, three states: the gate instead of a
/// listing, a refusal that says so, and the listing once the right password
/// lands. The middle step is the one worth the wall-clock — a wrong password
/// that merely hung would look identical to a slow connect.
fn cell_password(ctx: &Ctx<'_>) -> Res<()> {
    let page = Page::open_protected(ctx, 3, "", Some(PASSWORD))?;

    // The link alone shows nothing. Asserted before anything is typed: if the
    // listing were reachable here, the feature would not exist.
    wait_for_true(
        "/Password required/.test(document.body.innerText)",
        CONNECT_TIMEOUT,
        "the password gate",
    )?;
    if evaluate("String(/blob\\.bin/.test(document.body.innerText))")?.trim() == "true" {
        return Err("the share listed its files without a password".into());
    }

    // A wrong password is refused *as a wrong password*.
    type_password("not-the-password")?;
    click("Open share")?;
    wait_for_true(
        "/does not open this share/.test(document.body.innerText)",
        ACTION_TIMEOUT,
        "the gate to report a refused password",
    )?;

    // And the right one opens it.
    type_password(PASSWORD)?;
    click("Open share")?;
    wait_for_listing()?;

    page.finish()
}

/// **A wrong password is named with no producer at all.**
///
/// The case the whole design turns on. A share is built to outlive its
/// producer — seeders keep it alive — so a check that needs the producer to
/// answer is a check that usually cannot run. Before the ticket carried a mesh
/// id this hung: the tab redialled a dead origin, then waited out the seeder
/// deadline on a mesh a wrong password cannot even find, and never reached the
/// gate.
///
/// The producer is killed *before* the window ever opens, so nothing here can
/// be answered by the network.
fn cell_password_no_producer(ctx: &Ctx<'_>) -> Res<()> {
    let (dir, _blob_sha256) = make_share(3)?;
    let (mut producer, ticket) = serve(ctx.binary, dir.path(), Some(PASSWORD))?;
    // Dead before the browser is even launched.
    producer.interrupt();

    let target = format!("{}files/{ticket}", ctx.url);
    run_browse(&["launch", "--headless", ctx.folder, &target])?;
    reap::track_browser(ctx.folder);
    let _browser = Browser::new(ctx.folder.to_owned());
    run_browse(&[
        "wait",
        "--selector",
        "body",
        "--timeout",
        "60000",
        "--folder",
        ctx.folder,
    ])?;

    wait_for_true(
        "/Password required/.test(document.body.innerText)",
        CONNECT_TIMEOUT,
        "the password gate",
    )?;
    type_password("not-the-password")?;
    click("Open share")?;
    // A minute, not the full action timeout: the point is that this answer
    // comes from an Argon2id stretch and a byte comparison, not from a network
    // round trip that has nobody on the other end.
    wait_for_true(
        "/does not open this share/.test(document.body.innerText)",
        Duration::from_mins(1),
        "the gate to name the password with no producer running",
    )?;
    Ok(())
}

/// **An agent reads the share through the tools the page publishes.**
///
/// The bytes are the assertion. A tool that listed the right names while
/// returning the wrong window, or that reported UTF-8 for a binary read, would
/// hand a model a confident wrong answer — and every layer below here would
/// still be green, because the manifest and the transport were never at fault.
fn cell_webmcp_read(ctx: &Ctx<'_>) -> Res<()> {
    let page = Page::open(ctx, 3, "")?;
    wait_for_listing()?;

    // Where the blob's first NUL falls is a property of the fixture, not
    // something to assume: `looksBinary` is a NUL test, deliberately, so a
    // window that happens to hold none is *correctly* returned as text. Reading
    // the offset off disk aims the binary read at bytes that must be base64,
    // and gives something exact to compare the decode against.
    let blob = std::fs::read(page.dir.path().join("blob.bin"))
        .map_err(|error| format!("reading the fixture blob: {error}"))?;
    let Some(nul) = blob.iter().position(|byte| *byte == 0) else {
        return Err("the fixture blob holds no NUL byte to read".into());
    };
    let end = (nul + 8).min(blob.len());
    let binary_len = end - nul;
    // Compared as the byte array both sides already have, rather than hex — the
    // page decodes with `atob` and `check` stringifies, so `[17,0,…]` needs no
    // encoding step on either end.
    let expected_bytes = serde_json::to_string(&blob[nul..end])
        .map_err(|error| format!("describing the expected blob bytes: {error}"))?;

    run_webmcp(&format!(
        r"
        const opened = await call('shareConnect');
        check('shareConnect counts the share', [opened.ok, opened.files, opened.bytes],
              [true, 4, {bytes}]);

        const listed = await call('shareList');
        check('shareList names every entry', listed.entries.map((e) => e.name).sort(),
              ['blob.bin', 'f000.txt', 'f001.txt', 'f002.txt']);

        const stat = await call('shareStat', {{ path: 'f000.txt' }});
        check('shareStat sizes a file', [stat.kind, stat.size], ['file', {file_len}]);

        const whole = await call('shareRead', {{ path: 'f000.txt' }});
        check('shareRead returns the fixture bytes', [whole.encoding, whole.text, whole.eof],
              ['utf8', 'file 0 contents\n', true]);

        // One byte from the middle: the offset must reach the wire, not just
        // slice a window the page had already pulled in full.
        const window = await call('shareRead', {{ path: 'f000.txt', offset: 5, length: 1 }});
        check('shareRead honours offset and length',
              [window.text, window.eof, window.nextOffset], ['0', false, 6]);

        // Aimed at the blob's first NUL, so this window must not come back as
        // text — and the decode is compared byte for byte, not just typed.
        const binary = await call('shareRead',
                                  {{ path: 'blob.bin', offset: {nul}, length: {binary_len} }});
        const decoded = typeof binary.data === 'string'
          ? [...atob(binary.data)].map((char) => char.charCodeAt(0))
          : null;
        check('shareRead base64s a window holding a NUL',
              [binary.encoding, binary.text, decoded], ['base64', undefined, {expected_bytes}]);

        const found = await call('shareSearch', {{ query: 'contents' }});
        check('shareSearch finds every text file',
              found.matches.map((m) => m.path).sort(),
              ['f000.txt', 'f001.txt', 'f002.txt']);

        // What it left out, and why. A search that silently dropped the blob
        // would read as 'there is nothing else', which is a different answer.
        check('shareSearch says it skipped the binary',
              (found.skipped ?? []).map((s) => [s.path, s.reason]),
              [['blob.bin', 'binary']]);

        // The grammar is `*`, `**` and `?` only — a character class would be
        // matched literally, so this narrows with the wildcard that exists.
        const globbed = await call('shareSearch', {{ query: 'contents', glob: 'f000.*' }});
        check('shareSearch honours a glob',
              globbed.matches.map((m) => m.path), ['f000.txt']);
        ",
        bytes = 3 * FIXTURE_FILE_LEN + BLOB_LEN,
        file_len = FIXTURE_FILE_LEN,
    ))?;

    page.finish()
}

/// **Bad input comes back as a result, never as a throw.**
///
/// Two measured browser behaviours make this load-bearing rather than tidy, and
/// both are invisible to a unit test that calls `execute` directly: the browser
/// does not check a call against `inputSchema`, so a missing `required` field
/// arrives as `undefined`; and anything a tool throws is flattened to
/// `UnknownError`, losing the message. A tool that leaned on either would look
/// correct in isolation and tell an agent nothing here.
fn cell_webmcp_failures(ctx: &Ctx<'_>) -> Res<()> {
    let page = Page::open(ctx, 3, "")?;
    wait_for_listing()?;

    run_webmcp(
        r"
        await call('shareConnect');

        const missing = await call('shareStat', { path: 'nope.txt' });
        check('a path that is not there', [missing.ok, missing.code], [false, 'not_found']);

        // `path` is `required`, and the browser lets the call through without it.
        const noPath = await call('shareRead', {});
        check('a missing required argument', [noPath.ok, noPath.code], [false, 'bad_argument']);

        const escaping = await call('shareStat', { path: '../../etc/passwd' });
        check('a path that escapes the share root',
              [escaping.ok, escaping.code], [false, 'bad_argument']);

        const tooSmall = await call('shareRead', { path: 'f000.txt', length: 0 });
        check('a length under the schema minimum',
              [tooSmall.ok, tooSmall.code], [false, 'bad_argument']);

        // Every failure has to carry prose as well as a code — the code is for
        // the caller, the sentence is what the model acts on.
        check('every failure explains itself',
              [missing, noPath, escaping, tooSmall].every((r) => typeof r.error === 'string'
                                                                 && r.error.length > 0),
              true);
        ",
    )?;

    page.finish()
}

/// **The interface tools actually move the page.**
///
/// The half no unit test can reach. `shareNavigate` and `shareOpenView` return
/// a snapshot, and a version of them that built the snapshot without touching
/// the app would satisfy every assertion about their return value — so what is
/// checked here is the route the person is left on, read back from the page.
///
/// It ends on `/info`, which is also where the call log lives, so the last
/// assertion is that the page can see the traffic this row just made.
fn cell_webmcp_ui(ctx: &Ctx<'_>) -> Res<()> {
    let page = Page::open(ctx, 3, "")?;
    wait_for_listing()?;

    run_webmcp(
        r"
        const before = await call('shareUiState');
        check('the page starts on the file browser', before.view, 'files');

        // `selection` is one segment per level, not a joined path.
        const moved = await call('shareNavigate', { path: 'f001.txt' });
        check('shareNavigate selects a file', [moved.ok, moved.selection], [true, ['f001.txt']]);

        const opened = await call('shareOpenView', { view: 'info' });
        check('shareOpenView reports success', opened.ok, true);
        ",
    )?;

    wait_for_true(
        "location.pathname.startsWith('/info/')",
        ACTION_TIMEOUT,
        "shareOpenView to move the page to the info panel",
    )?;
    // The log panel, fed by the calls above rather than by a fixture.
    wait_for_true(
        "/tools published/.test(document.body.innerText)",
        ACTION_TIMEOUT,
        "the WebMCP panel to report the tools this page published",
    )?;

    run_webmcp(
        r"
        const back = await call('shareOpenView', { view: 'files' });
        check('shareOpenView goes back', back.ok, true);
        ",
    )?;
    wait_for_true(
        "location.pathname.startsWith('/files/')",
        ACTION_TIMEOUT,
        "shareOpenView to return the page to the file browser",
    )?;

    page.finish()
}
