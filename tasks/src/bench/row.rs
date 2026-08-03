//! The result schema `cargo task bench` writes, and its two renderers.
//!
//! `docs/rfc/02-performance.md` blames the previous, deleted numbers on missing
//! provenance: "every row states machine, OS, browser version, transport,
//! direction and RTT". So the environment is captured once per run in
//! [`Provenance`] and the per-cell facts live on [`Row`] — including **its own
//! measured RTT**, which is this harness's substitute for injecting a delay.

use std::fmt::Write as _;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::proc::Res;

/// `agent-share bench --output json` prints exactly this and nothing else
/// (`mount/bench.rs:576-580`), so the harness deserializes stdout whole.
/// Mirrors `BenchReport` at `mount/bench.rs:78-87`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct BenchReport {
    pub(crate) transport: String,
    /// Fill requests kept in flight; `1` is the historical serial shape.
    #[serde(default = "one")]
    pub(crate) depth: usize,
    pub(crate) connect_ms: f64,
    pub(crate) duration_s: f64,
    pub(crate) latency_ms: LatencyStats,
    pub(crate) throughput_mib_s: f64,
    pub(crate) bytes: u64,
    pub(crate) pings: u32,
}

/// Older bench binaries did not report a depth; they were always serial.
fn one() -> usize {
    1
}

/// Mirrors `LatencyStats` at `mount/bench.rs:71-76`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct LatencyStats {
    pub(crate) min: f64,
    pub(crate) median: f64,
    pub(crate) p95: f64,
}

/// The environment every row in one run shares.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Provenance {
    pub(crate) tag: String,
    pub(crate) git_sha: String,
    pub(crate) git_dirty: bool,
    pub(crate) machine: String,
    pub(crate) os: String,
    pub(crate) rustc: String,
    pub(crate) chrome: Option<String>,
    pub(crate) wasm_bytes: Option<u64>,
    pub(crate) corpus_mib: u64,
    pub(crate) duration_s: u64,
    pub(crate) repeats: usize,
}

/// What a cell cost in CPU, both sides.
///
/// This is the column that separates "waiting on the network" from "burning a
/// core", which is the first fork in `docs/rfc/02-performance.md`'s Tier 1 —
/// finding #1 predicts idle waiting, findings #2/#3 predict work. The
/// `user`/`sys` split separates userspace math (ciphers, copies) from kernel
/// time (syscalls, context switches), and settles whether double encryption is
/// worth chasing without needing a profiler.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Cpu {
    /// Only available where the consumer runs to completion under
    /// `/usr/bin/time`; a long-lived consumer we interrupt reports a total.
    pub(crate) consumer_user_s: Option<f64>,
    pub(crate) consumer_sys_s: Option<f64>,
    pub(crate) consumer_cpu_s: Option<f64>,
    pub(crate) producer_cpu_s: Option<f64>,
    /// CPU seconds per wall second across both processes. Above 1.0 means more
    /// than a whole core is busy moving these bytes.
    pub(crate) cores_busy: Option<f64>,
}

impl Cpu {
    /// Both sides sampled, with the consumer's userspace/kernel split known.
    pub(crate) fn from_split(
        consumer: Option<(f64, f64)>,
        producer_cpu_s: Option<f64>,
        wall_s: f64,
    ) -> Self {
        let mut cpu = Self::from_totals(
            consumer.map(|(user, sys)| user + sys),
            producer_cpu_s,
            wall_s,
        );
        cpu.consumer_user_s = consumer.map(|(user, _)| user);
        cpu.consumer_sys_s = consumer.map(|(_, sys)| sys);
        cpu
    }

    /// Both sides sampled by cumulative total only — the shape available when
    /// the consumer is a long-lived process we interrupt rather than await.
    pub(crate) fn from_totals(
        consumer_cpu_s: Option<f64>,
        producer_cpu_s: Option<f64>,
        wall_s: f64,
    ) -> Self {
        let cores_busy = if wall_s > 0.0 && (consumer_cpu_s.is_some() || producer_cpu_s.is_some()) {
            Some((consumer_cpu_s.unwrap_or(0.0) + producer_cpu_s.unwrap_or(0.0)) / wall_s)
        } else {
            None
        };
        Self {
            consumer_user_s: None,
            consumer_sys_s: None,
            consumer_cpu_s,
            producer_cpu_s,
            cores_busy,
        }
    }
}

/// One cell's outcome: either a measurement or an explicit, reasoned skip.
///
/// A skipped cell is still a row. A cell that silently vanished is how a matrix
/// comes to read as "covered everything" when it did not.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Row {
    pub(crate) cell: String,
    pub(crate) transport: String,
    pub(crate) direction: String,
    /// Whether a gossip mesh shared this row's endpoint. The mesh runs on the
    /// *same* iroh endpoint and congestion domain as the data path
    /// (`mount/produce.rs:124-127`), so a row that does not say is measuring
    /// two things at once.
    pub(crate) mesh: String,
    /// Fill requests kept in flight for this row.
    pub(crate) depth: usize,
    /// The row's own measured RTT — `latency_ms.median` of the median run.
    /// This is the column that replaces `dnctl`/`pfctl` delay injection.
    pub(crate) rtt_ms: Option<f64>,
    pub(crate) throughput_mib_s: Option<f64>,
    pub(crate) connect_ms: Option<f64>,
    pub(crate) latency_ms: Option<LatencyStats>,
    pub(crate) bytes: Option<u64>,
    pub(crate) duration_s: Option<f64>,
    /// Every repeat's throughput, so a reader can judge the spread themselves.
    pub(crate) samples_mib_s: Vec<f64>,
    /// `(max - min) / median`, as a percentage. The RFC wants three runs
    /// reproducible "within noise"; above [`SPREAD_WARN_PCT`] they are not.
    pub(crate) spread_pct: Option<f64>,
    pub(crate) cpu: Option<Cpu>,
    pub(crate) skipped: Option<String>,
    pub(crate) notes: Vec<String>,
}

/// Above this, the run prints a warning rather than quietly reporting a median
/// nobody can reproduce.
pub(crate) const SPREAD_WARN_PCT: f64 = 15.0;

impl Row {
    /// A cell that could not run, and why. Never silently dropped.
    pub(crate) fn skipped(cell: &str, transport: &str, direction: &str, reason: &str) -> Self {
        Self {
            cell: cell.to_owned(),
            transport: transport.to_owned(),
            direction: direction.to_owned(),
            mesh: "n/a".to_owned(),
            depth: 1,
            rtt_ms: None,
            throughput_mib_s: None,
            connect_ms: None,
            latency_ms: None,
            bytes: None,
            duration_s: None,
            samples_mib_s: Vec::new(),
            spread_pct: None,
            cpu: None,
            skipped: Some(reason.to_owned()),
            notes: Vec::new(),
        }
    }

    /// Attach the CPU split measured across this cell's window.
    #[must_use]
    pub(crate) fn with_cpu(mut self, cpu: Cpu) -> Self {
        self.cpu = Some(cpu);
        self
    }

    /// Fold N repeats of the same cell into one row, keeping the median run's
    /// full report and every run's throughput.
    pub(crate) fn from_reports(
        cell: &str,
        direction: &str,
        mesh: &str,
        reports: Vec<BenchReport>,
        notes: Vec<String>,
    ) -> Self {
        let mut ordered = reports;
        ordered.sort_by(|left, right| {
            left.throughput_mib_s
                .partial_cmp(&right.throughput_mib_s)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let samples: Vec<f64> = ordered.iter().map(|rep| rep.throughput_mib_s).collect();
        let middle = ordered
            .get(ordered.len() / 2)
            .expect("from_reports called with at least one report")
            .clone();

        Self {
            cell: cell.to_owned(),
            transport: middle.transport.clone(),
            direction: direction.to_owned(),
            mesh: mesh.to_owned(),
            depth: middle.depth,
            rtt_ms: Some(middle.latency_ms.median),
            throughput_mib_s: Some(middle.throughput_mib_s),
            connect_ms: Some(middle.connect_ms),
            latency_ms: Some(middle.latency_ms),
            bytes: Some(middle.bytes),
            duration_s: Some(middle.duration_s),
            spread_pct: spread_pct(&samples),
            cpu: None,
            samples_mib_s: samples,
            skipped: None,
            notes,
        }
    }

    /// A cell whose product is wall-clock over a known byte count rather than a
    /// `BenchReport` — the `cp` through a real mount.
    pub(crate) fn from_transfers(
        cell: &str,
        transport: &str,
        direction: &str,
        mesh: &str,
        samples: Vec<f64>,
        bytes: u64,
        notes: Vec<String>,
    ) -> Self {
        let mut ordered = samples.clone();
        ordered.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
        let median = ordered.get(ordered.len() / 2).copied();

        Self {
            cell: cell.to_owned(),
            transport: transport.to_owned(),
            direction: direction.to_owned(),
            mesh: mesh.to_owned(),
            depth: 1,
            rtt_ms: None,
            throughput_mib_s: median,
            connect_ms: None,
            latency_ms: None,
            bytes: Some(bytes),
            duration_s: None,
            spread_pct: spread_pct(&samples),
            cpu: None,
            samples_mib_s: samples,
            skipped: None,
            notes,
        }
    }
}

/// `(max - min) / median` as a percentage; `None` for fewer than two samples.
fn spread_pct(samples: &[f64]) -> Option<f64> {
    if samples.len() < 2 {
        return None;
    }
    let mut ordered = samples.to_vec();
    ordered.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    let low = *ordered.first()?;
    let high = *ordered.last()?;
    let median = *ordered.get(ordered.len() / 2)?;
    if median <= 0.0 {
        return None;
    }
    Some((high - low) / median * 100.0)
}

/// A whole run: the shared environment plus every cell's row.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Report {
    pub(crate) provenance: Provenance,
    pub(crate) rows: Vec<Row>,
}

impl Report {
    /// Write `docs/perf/<tag>.json` and regenerate `docs/perf/README.md`.
    pub(crate) fn write(&self, perf_dir: &Path) -> Res<()> {
        std::fs::create_dir_all(perf_dir)
            .map_err(|error| format!("create {}: {error}", perf_dir.display()))?;

        let json_path = perf_dir.join(format!("{}.json", self.provenance.tag));
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&json_path, format!("{json}\n"))
            .map_err(|error| format!("write {}: {error}", json_path.display()))?;

        let markdown = self.markdown();
        let table_path = perf_dir.join(format!("{}.md", self.provenance.tag));
        std::fs::write(&table_path, &markdown)
            .map_err(|error| format!("write {}: {error}", table_path.display()))?;

        // `README.md` is the baseline's table, not the last run's. A one-cell
        // diagnostic run must not silently replace the committed matrix with a
        // single row — that is exactly the "reads as if it covered everything"
        // failure the RFC calls out.
        if self.provenance.tag == "baseline" {
            let readme_path = perf_dir.join("README.md");
            std::fs::write(&readme_path, &markdown)
                .map_err(|error| format!("write {}: {error}", readme_path.display()))?;
        }
        Ok(())
    }

    /// The human-readable table. Provenance rides above it rather than being
    /// repeated into twenty columns — but it is never absent.
    fn markdown(&self) -> String {
        let prov = &self.provenance;
        let mut out = String::new();
        let _ = writeln!(out, "# agent-share performance baseline\n");
        let _ = writeln!(
            out,
            "Generated by `cargo task bench --tag {}`. Do not hand-edit — rerun it.\n",
            prov.tag
        );
        let _ = writeln!(out, "## Provenance\n");
        let dirty = if prov.git_dirty {
            " (dirty working tree — numbers are not reproducible from this SHA)"
        } else {
            ""
        };
        let _ = writeln!(out, "- commit: `{}`{dirty}", prov.git_sha);
        let _ = writeln!(out, "- machine: {}", prov.machine);
        let _ = writeln!(out, "- os: {}", prov.os);
        let _ = writeln!(out, "- rustc: {}", prov.rustc);
        let _ = writeln!(
            out,
            "- chrome: {}",
            prov.chrome.as_deref().unwrap_or("not measured")
        );
        let _ = writeln!(
            out,
            "- wasm: {}",
            prov.wasm_bytes
                .map_or_else(|| "not built".to_owned(), |bytes| format!("{bytes} bytes"))
        );
        let _ = writeln!(
            out,
            "- corpus: {} MiB · bench window: {} s · repeats: {}\n",
            prov.corpus_mib, prov.duration_s, prov.repeats
        );

        let _ = writeln!(out, "## Results\n");
        let _ = writeln!(
            out,
            "| cell | transport | direction | depth | RTT (ms) | MiB/s | connect (ms) | cores busy | user/sys (s) | spread |"
        );
        let _ = writeln!(out, "|---|---|---|---:|---:|---:|---:|---:|---:|---:|");
        for row in &self.rows {
            if let Some(reason) = &row.skipped {
                let _ = writeln!(
                    out,
                    "| `{}` | {} | {} | — | — | **skipped** | — | — | — | {reason} |",
                    row.cell, row.transport, row.direction
                );
                continue;
            }
            let cpu = row.cpu.as_ref();
            let _ = writeln!(
                out,
                "| `{}` | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                row.cell,
                row.transport,
                row.direction,
                row.depth,
                fmt2(row.rtt_ms),
                fmt2(row.throughput_mib_s),
                fmt1(row.connect_ms),
                fmt2(cpu.and_then(|usage| usage.cores_busy)),
                cpu.map_or_else(
                    || "—".to_owned(),
                    |usage| format!(
                        "{} / {}",
                        fmt1(usage.consumer_user_s),
                        fmt1(usage.consumer_sys_s)
                    )
                ),
                row.spread_pct
                    .map_or_else(|| "—".to_owned(), |pct| format!("{pct:.0}%")),
            );
        }
        let _ = writeln!(
            out,
            "\n`cores busy` is CPU-seconds per wall-second across producer and \
             consumer: above 1.0 the cell is compute-bound, not waiting on the \
             network. `user/sys` is the consumer's split — ciphers and copies \
             land in `user`, syscalls and context switches in `sys`.\n"
        );
        let _ = writeln!(out, "Mesh state per row:\n");
        for row in &self.rows {
            let _ = writeln!(out, "- `{}`: {}", row.cell, row.mesh);
        }

        let notes: Vec<&Row> = self
            .rows
            .iter()
            .filter(|row| !row.notes.is_empty())
            .collect();
        if !notes.is_empty() {
            let _ = writeln!(out, "\n## Notes\n");
            for row in notes {
                for note in &row.notes {
                    let _ = writeln!(out, "- `{}`: {note}", row.cell);
                }
            }
        }
        out
    }
}

fn fmt1(value: Option<f64>) -> String {
    value.map_or_else(|| "—".to_owned(), |num| format!("{num:.1}"))
}

fn fmt2(value: Option<f64>) -> String {
    value.map_or_else(|| "—".to_owned(), |num| format!("{num:.2}"))
}
