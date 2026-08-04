# S0.1 — is `bao-tree` wasm-clean?

**Verdict: holds.** Raw capture: [`../data/s01-wasm-build.txt`](../data/s01-wasm-build.txt).

## Why it mattered

The entire premise of `fofoca-blobs` is that `bao-tree` gives us iroh-blobs'
verification core without iroh-blobs' store. If `bao-tree` could not build for
`wasm32-unknown-unknown`, browser verification would need another path and the
crate's justification would weaken sharply.

Cost to falsify: about an hour. Ran first for that reason.

## What was established

- **[measured]** Builds for `wasm32-unknown-unknown` with
  `default-features = false, features = ["validate"]`.
- **[measured]** The cdylib has **zero imports** — not merely no
  `import "env"`, but none at all. Fully self-contained.
- **[measured]** The wasm dependency set is 14 crates with **no `iroh-io` and
  no `tokio`**. `iroh-blobs` would add roughly 15 more on top, plus a second
  ALPN and a `[patch.crates-io]` entry.
- **[measured]** Correctness, not just compilation — five tests pass natively:
  full round-trip, **partial-range verification against the same root**,
  **tamper rejection**, outboard overhead, and hash agreement with blake3.
- **[measured]** Both wasm variants execute under node and produce **identical
  checksums** (`16818158`), so the SIMD backend agrees with the portable one
  bit-for-bit.

The partial-range and tamper tests matter beyond this spike: they are the two
properties the entire swarm-trust argument rests on. A peer can serve a range it
holds, verifiable against the root, and a flipped bit fails.

## Correction to RFC 01

**[verified]** RFC 01 says "`bao-tree` minus its `fs` feature is wasm-clean".
The verified default set is `["tokio_fsm", "validate", "serde", "fs"]`, and
**`tokio_fsm` pulls `iroh-io`**, which is std/fs-bound. The recipe is
`default-features = false` plus selectively re-adding what is needed — not
dropping `fs`.

## Method note

The first cdylib build was **367 bytes with 0 imports**, and the import check
passed. It was vacuous: a Rust cdylib exports only `extern "C"` symbols, so the
linker had stripped every `pub fn` in the spike. Adding a
`#[unsafe(no_mangle)] pub extern "C"` entry point that exercises outboard
construction, range encode, decode+verify, `valid_ranges` and hashing took the
module to 122 KB.

**A wasm build that merely depends on a crate proves nothing about that crate.**

## Still assumed

- That the same feature set stays wasm-clean as `bao-tree` moves. Cheap to
  re-check; the spike is throwaway and regenerable.
- That `genawaiter` (pulled by `validate`) behaves in a browser engine, not just
  under node. It is a generator shim with no I/O, so this is low-risk, but it
  has not been exercised in Safari or Chrome.
