# Stage 0 harnesses

The code that produced [`../data/`](../data/). Kept so the numbers can be
re-derived rather than trusted, and so the two **open** verifications
([S0.4](../findings/s04-multi-source-throughput.md) delayed-link,
[S0.5](../findings/s05-opfs-worker.md) in Chrome) can actually be run.

These are spikes, not products: no CI, no lint gate, throwaway quality by
design. They are excluded from the cargo workspace by their own `[workspace]`
stanza, so they neither build nor break with the repo.

**The harness that is *not* here** lives in the repo proper, because it earned
a permanent place:

- `crates/agent-share/src/mount/mod.rs` — `a_non_origin_peer_serves_the_origins_ticket_secret`
  and `a_diverged_peer_answers_plausibly_and_wrongly` (S0.2). Permanent
  regression guards; they run in the normal suite.
- `crates/agent-share/src/mount/bench.rs` — `s04_multi_source_throughput_scaling`
  (S0.4). `#[ignore]`d measurement.

## Contents

| Path | Spike | What it answers |
|---|---|---|
| `baotree-spike/` | S0.1, S0.3 | Does `bao-tree` build and run wasm-clean, is it correct, and how fast is outboard construction native vs wasm? |
| `wasm_imports.py` | S0.1 | Dumps a `.wasm` import section. `wasm-tools` was not installed; the section is trivial to parse. |
| `run_wasm.mjs` | S0.1 | Executes the spike module under node; proves portable and SIMD agree bit-for-bit. |
| `bench_wasm.mjs` | S0.3 | Portable vs `+simd128`, hash-only and outboard-only. |
| `opfs/` | S0.5 | Browser half: OPFS random-access range writes from a Worker, and persistence across reload. Plain JS. |
| `opfs-rust-bindings/` | S0.5 | Rust half: does `web-sys` expose `FileSystemSyncAccessHandle` with read/write at an offset? Compile-check only. |
| `s04-delayed-link.sh` | S0.4 | Runs the multi-source measurement under an emulated delayed link. **Needs root.** |

## Running them

**S0.1 / S0.3** — from `baotree-spike/`:

```
cargo test --release -- --nocapture              # correctness + outboard overhead
cargo run  --release --features rayon -- 256     # native throughput table
cargo build --release --target wasm32-unknown-unknown
RUSTFLAGS="-C target-feature=+simd128" \
  cargo build --release --target wasm32-unknown-unknown --features wasm-simd
```

Then, with the two `.wasm` files copied alongside as `portable.wasm` /
`simd.wasm`:

```
python3 ../wasm_imports.py portable.wasm         # expect: 0 imports
node ../bench_wasm.mjs 64 5                      # portable vs simd
```

> **A dependency-only wasm build proves nothing.** The first attempt produced a
> 367-byte module with 0 imports because nothing was `extern "C"` and the linker
> stripped every path. `src/lib.rs` carries a forced entry point for this
> reason — do not remove it.

**S0.5 browser half** — OPFS needs a secure context, and `localhost` counts:

```
cd opfs && python3 -m http.server 8777
```

Open `http://localhost:8777/`, click Run, then **reload and click Run again**.
The second run must report `PASS persistence`. Do this in **Chrome**; Safari 27
is already recorded in [`../data/s05-opfs-safari.txt`](../data/s05-opfs-safari.txt).

**S0.5 Rust half** — from `opfs-rust-bindings/`:

```
cargo check --target wasm32-unknown-unknown
```

**S0.4 delayed link** — needs root, and rewrites pf rules:

```
sudo ./s04-delayed-link.sh 25 5 10               # delay_ms reps secs
```

Scoped to **UDP on `lo0` only** — every peer in the test is on loopback, so it
cannot touch real traffic. It saves pf state and restores it via a trap on any
exit including Ctrl-C, and **refuses to run** if pf is already enabled with
rules rather than clobbering them (`FORCE=1` overrides). It runs cargo as
`$SUDO_USER` so `target/` does not end up root-owned.

> **Read the ping result and the "measured median RTT" line before anything
> else.** If RTT still reads ~0.3 ms the shaping never reached the test's
> traffic and the throughput numbers are just the unshaped baseline. macOS
> dummynet may not apply to `lo0`; if so the honest next step is two physical
> machines, not more tuning.
