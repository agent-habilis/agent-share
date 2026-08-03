# Can the harness see a change at all?

`README.md` and `baseline.json` are generated — rerun `cargo task bench` to
change them. **This file is hand-written** and records the sensitivity checks
that decide whether the generated numbers are worth anything.

`docs/rfc/02-performance.md` sets the bar: "flip `opt-level = "s"` → `3`,
re-run, confirm the wasm numbers move… **If it cannot see that, it cannot see
anything**." Both named canaries were run. Both came back null — and the reason
is that the RFC's premise was wrong, not that the harness is blind.

## Canary 1 — wasm `opt-level = "s"` → `3`

`crates/agent-share-wasm-client/Cargo.toml`, rebuilt with `cargo task web-wasm`,
measured on `browser-consume-webrtc`, 2 repeats each, reverted afterwards.

| | `.wasm` bytes | MiB/s | samples |
|---|---:|---:|---|
| `opt-level = "s"` | 7,381,312 | 21.36 | 21.05, 21.36 |
| `opt-level = 3` | 8,849,342 | 21.69 | 21.44, 21.69 |

**+20% binary size for +1.5% throughput** — inside the noise floor.

This is a real finding, not a failed measurement. The RFC's SIMD section ranks
`opt-level = "s"` → `3` as lever #1 ("Nothing else on this list works through
it"). On the browser byte path it buys nothing, because that path is not bound
by wasm codegen quality — `browser-consume-webrtc` (19.7 MiB/s) and
`native-synth-webrtc` (19.1 MiB/s) land in the same place, and the native leg
runs no wasm at all. The shared WebRTC datagram path is the constraint.

The size cost is exactly what the RFC's edge case #10 predicted.

## Canary 2 — NFS `rsize` 131072 → 262144

`crates/agent-share/src/mount/consume.rs:588-599`, measured on
`native-mount-cp`, 3 repeats, reverted afterwards.

| | MiB/s | samples | spread |
|---|---:|---|---:|
| `rsize=131072` | 239.88 | — | 4% |
| `rsize=262144` | 246.55 | 249.4, 244.5, 246.5 | 2% |

**+2.8%**, marginally outside a 2–4% noise floor. Doubling the request size
barely registers on a host-local QUIC path, which says that path is not
request-bound.

The RFC's other named native canary, quinn's `stream_receive_window`, was
**not** run, because at the depth this harness currently drives it cannot
possibly bind: the window is 1.25 MB and the largest single request in flight is
`MAX_BENCH_FILL_BYTES` = 1 MiB. The RFC says as much itself — it "binds the
moment read depth rises". It becomes a meaningful canary in Phase 1, once depth
is raised, and not before.

## So what *does* demonstrate sensitivity?

Changing one flag on one cell:

| cell | transport | MiB/s | spread |
|---|---|---:|---:|
| `native-mount-cp` | quic | 245.82 | 3% |
| `native-mount-cp-webrtc` | webrtc | 3.47 | 130% |

Same NFS client, same `rsize`, same producer code, same host — **63–71×**,
reproducible. `consume.rs:383-386` calls `ensure_webrtc_selected` and errors if
anything but a direct WebRTC path was chosen, so this is not a relay fallback
in disguise.

Alongside that, the harness has already caught things nobody asked it to:

- a **61% spread** traced to an unrelated `cargo test` running in a sibling
  worktree — the noise guard working as intended;
- a **stale-wasm/fresh-glue mismatch** in the browser leg (Chrome cached the
  `.wasm` at its fixed URL while Bun re-bundled the glue), surfaced *by* canary
  1 and fixed with `Network.clearBrowserCache`;
- **`performance.now()` clamping**, which makes the browser rows report
  `RTT 0.00`. Recorded as unmeasured rather than as zero, per the RFC's edge
  case #5.

## Honest limits of this baseline

- **Three rows are noisy** and say so: `native-mount-cp-webrtc` (130%),
  `browser-consume-relay` (174%), `native-synth-webrtc` (21%). Their medians are
  indicative only. The relay rows depend on a shared public relay whose
  bandwidth is not ours to control, so their throughput is bounded by more than
  RTT.
- **RTT is measured, not injected.** There is no controlled high-RTT point;
  `dnctl`/`pfctl` would need root and were deliberately avoided.
- **`native-mount-cp-webrtc` differs from its control in discovery** as well as
  transport — WebRTC signalling needs a relay to broker it, so that leg cannot
  use a loopback swarm. Both move data over a host-local path.
- **Browser-as-producer covers only the synthetic `BenchProducer`.** A browser
  serving real files needs the File System Access picker, which requires a user
  gesture, so the `read_from_handle` path the RFC wants exercised is still not
  covered by any automated cell.
- The browser bench window is **pinned at 30 s** (`web/src/lab.ts:102` passes
  `undefined`), so `--duration` does not reach browser cells.
