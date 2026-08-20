use std::process::ExitCode;

use clap::{Parser, Subcommand};
use xshell::Shell;

mod bench;
mod build;
mod ci;
mod clean;
mod coverage;
mod e2e;
mod fmt;
mod install;
mod lint;
mod man;
mod naming;
mod proptest;
mod release;
mod run;
mod test;
mod util;
mod web_image;
mod web_wasm;

/// Task result; any `Err` is printed and turns into a non-zero exit.
pub(crate) type TaskOutcome = Result<(), Box<dyn std::error::Error>>;

/// Project task runner. Run `cargo task <task>`.
#[derive(Parser)]
#[command(bin_name = "cargo task")]
struct Cli {
    #[command(subcommand)]
    task: Task,
}

/// Variant doc comments *are* the `--help` text — no separate usage
/// block to drift.
#[derive(Subcommand)]
enum Task {
    /// Run unit tests.
    Test,
    /// Build the `agent-share` binary. Cross-compile with `--target <triple>` or the
    /// `--arch <arch>` shorthand (glibc Linux) through a project-pinned
    /// zig + cargo-zigbuild toolchain — self-contained, never the global zig
    /// or a global `cargo install`.
    Build {
        /// Full target triple, e.g. `aarch64-unknown-linux-gnu`.
        #[arg(long)]
        target: Option<String>,
        /// Architecture shorthand ⇒ `<arch>-unknown-linux-gnu` (e.g.
        /// `aarch64`, `x86_64`). Mutually exclusive with `--target`.
        #[arg(long)]
        arch: Option<String>,
        /// Optimized release build (default: debug).
        #[arg(long)]
        release: bool,
    },
    /// Build the release binary.
    Release {
        /// `cargo-release` level (`patch`|`minor`|`major`|`x.y.z`) plus
        /// extra flags such as `--execute`. Dry run by default; with no
        /// args this just builds the release binary.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Run the binary (`cargo run`). Extra args go to `agent-share`
    /// (e.g. `cargo task run serve ./dir`).
    Run {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Install the binary.
    Install,
    /// Run the performance matrix and write `docs/perf/`. Needs no input and
    /// no privileges: RTT is measured per row, not injected.
    Bench {
        /// Seconds per bench window (native cells only — the browser's is
        /// pinned at 30 s by `packages/agent-share-web/src/pages/lab/consumer.tsx`).
        #[arg(long, default_value_t = 15)]
        duration: u64,
        /// Runs per cell; the row reports the median and the spread.
        #[arg(long, default_value_t = 3)]
        repeats: usize,
        /// Size of the generated corpus the mount cell transfers.
        #[arg(long = "corpus-mib", default_value_t = 1024)]
        corpus_mib: u64,
        /// `all`, or a comma-separated subset of cell names.
        #[arg(long, default_value = "all")]
        cells: String,
        /// Comma-separated fill depths to sweep on the synthetic cells, e.g.
        /// `1,2,4,8`. Depth 1 is strictly serial and is what the committed
        /// baseline measures.
        #[arg(long, default_value = "1", value_delimiter = ',')]
        depths: Vec<usize>,
        /// Names the output file: `docs/perf/<tag>.json`.
        #[arg(long, default_value = "baseline")]
        tag: String,
    },
    /// Drive the web app in a headless browser against a real producer. Not in
    /// the gate: it needs `agent-browse`, a built wasm and the network, and a
    /// missing prerequisite is reported as a skip rather than a pass. See
    /// `docs/testing.md`.
    E2e {
        /// `all`, or a comma-separated subset of cell names.
        #[arg(long, default_value = "all")]
        cells: String,
    },
    /// Run tests with coverage.
    Coverage,
    /// Run the CI gate.
    Ci,
    /// Format source files.
    Fmt,
    /// Run clippy lints.
    Lint,
    /// Remove build artifacts.
    Clean,
    /// Generate roff man pages into `target/man/` (needs `clap_mangen`).
    Man,
    /// Check file and directory names: snake_case inside a crate, kebab-case
    /// everywhere else. Reads `git ls-files`, so build output and ignored
    /// files are out of scope.
    Naming,
    /// Run property-based tests.
    Proptest,
    /// Build the browser/Node wasm client into `packages/agent-share-wasm`.
    WebWasm,
    /// Build the browser app into a container image — Bun serving the static `dist/` —
    /// and push it to the self-hosted Gitea registry. Hermetic: the image
    /// rebuilds the wasm from source, so nothing on this machine leaks into it
    /// and no `web-wasm` run is needed first.
    WebImage {
        /// Image tag. Defaults to the short commit sha, marked `-dirty` when
        /// the tree has uncommitted changes. `latest` is always tagged and
        /// pushed alongside it.
        #[arg(long)]
        tag: Option<String>,
        /// Registry host.
        #[arg(long, default_value = "srvc-gitea.tetra-ostrich.ts.net")]
        registry: String,
        /// Gitea user or org that owns the package.
        #[arg(long, default_value = "caiogondim")]
        owner: String,
        /// Build only — skip both pushes.
        #[arg(long)]
        no_push: bool,
        /// Target platform. The homelab and this Mac are both arm64, so nothing
        /// here emulates; changing it pulls in qemu and gets slow.
        #[arg(long, default_value = "linux/arm64")]
        platform: String,
    },
    /// Internal: cargo-zigbuild's `zig cc`/`c++`/`ar` shim. cargo-zigbuild's
    /// cross-link wrapper re-execs THIS binary as `<exe> zig …` (it resolves
    /// itself via `current_exe()`), so the cross build in `build` can only link
    /// if this arm exists. Not for human use.
    #[command(hide = true, subcommand)]
    Zig(cargo_zigbuild::Zig),
}

fn main() -> ExitCode {
    // cargo-zigbuild is a multi-call binary: for the archiver step it copies
    // THIS executable to `ar`/`lib`/`dlltool` and dispatches on argv[0]. When
    // the cross build invokes one of those copies, stand in for cargo-zigbuild
    // exactly as its own `main` does. (The `cc`/`c++`/`ranlib` wrappers are
    // instead scripts that call `<exe> zig …`, handled by `Task::Zig`.)
    if let Some(code) = run_as_zig_multicall() {
        return code;
    }

    let cli = Cli::parse();
    let sh = match Shell::new() {
        Ok(sh) => sh,
        Err(error) => {
            util::output::error(&error.to_string());
            return ExitCode::FAILURE;
        }
    };

    let outcome = match cli.task {
        Task::Test => test::run(&sh),
        Task::Build {
            target,
            arch,
            release,
        } => build::run(&sh, target.as_deref(), arch.as_deref(), release),
        Task::Bench {
            duration,
            repeats,
            corpus_mib,
            cells,
            depths,
            tag,
        } => bench::run(
            &sh,
            &bench::Options {
                duration,
                repeats,
                corpus_mib,
                cells,
                tag,
                depths,
            },
        ),
        Task::Release { args } => release::run(&sh, &args),
        Task::Run { args } => run::run(&sh, &args),
        Task::Install => install::run(&sh),
        Task::E2e { cells } => e2e::run(&sh, &cells),
        Task::Coverage => coverage::run(&sh),
        Task::Ci => ci::run(&sh),
        Task::Fmt => fmt::run(&sh),
        Task::Lint => lint::run(&sh),
        Task::Clean => clean::run(&sh),
        Task::Man => man::run(),
        Task::Naming => naming::run(&sh),
        Task::Proptest => proptest::run(&sh),
        Task::WebWasm => web_wasm::run(&sh),
        Task::WebImage {
            tag,
            registry,
            owner,
            no_push,
            platform,
        } => web_image::run(
            &sh,
            &web_image::Options {
                tag,
                registry,
                owner,
                no_push,
                platform,
            },
        ),
        Task::Zig(zig) => zig
            .execute()
            .map_err(|err| -> Box<dyn std::error::Error> { err.into() }),
    };

    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            util::output::error(&error.to_string());
            ExitCode::FAILURE
        }
    }
}

/// Mirror of cargo-zigbuild's `main` program-name dispatch: when this binary is
/// invoked under the name of a tool cargo-zigbuild copies itself to (`ar` /
/// `lib` / `dlltool` / `install_name_tool`), run that tool via the library and
/// return the exit code. Returns `None` for a normal `cargo task …` invocation.
fn run_as_zig_multicall() -> Option<ExitCode> {
    use cargo_zigbuild::Zig;

    let mut args = std::env::args();
    let program = args.next()?;
    let name = std::path::Path::new(&program)
        .file_stem()?
        .to_string_lossy()
        .into_owned();

    let result = if name.eq_ignore_ascii_case("ar") {
        Zig::Ar {
            args: args.collect(),
        }
        .execute()
    } else if name.eq_ignore_ascii_case("lib") {
        Zig::Lib {
            args: args.collect(),
        }
        .execute()
    } else if name.ends_with("dlltool") {
        Zig::Dlltool {
            args: args.collect(),
        }
        .execute()
    } else if name.eq_ignore_ascii_case("install_name_tool") {
        cargo_zigbuild::macos::install_name_tool::execute(args)
    } else {
        return None;
    };

    Some(match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            util::output::error(&error.to_string());
            ExitCode::FAILURE
        }
    })
}
