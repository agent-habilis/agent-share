/**
 * Build `crates/agent-share-wasm-client/` into the two wasm-bindgen outputs the
 * front ends consume, both of them inside `packages/agent-share-wasm/`:
 * `src/glue/` for this app and `node/` for the npx CLI. One `.wasm`, two glue
 * layers.
 *
 * They land in the package rather than in the crate's own `dist/` because both
 * consumers need them there. The dev bundler's watcher never leaves `packages/`,
 * so glue built outside it went stale across a rebuild and met the fresh binary
 * as `LinkError: … function import requires a callable`; that used to be patched
 * up by a mirror step. And `agent-share-node` imports `agent-share-wasm/node` by
 * name, which only resolves to something a published tarball can carry if the
 * files are in the package to begin with.
 *
 * This lives here, rather than in the task runner, so `bun run build` is
 * self-contained — the bundle's largest input used to come from a `cargo task`
 * somebody had to remember to run first, and a forgotten one either failed the
 * build outright or, worse, bundled the previous binary. `cargo task web-wasm`
 * now shells into this file, so there is one implementation and no drift.
 *
 * The crate is a standalone workspace, so every command below runs from its
 * directory rather than from the repo root.
 */

import { relative } from 'node:path'
import { fileURLToPath } from 'node:url'

import { $ } from 'bun'

import { GLUE_DIR, NODE_GLUE_DIR } from './wasm-asset.ts'

const REPO_ROOT = new URL('../', import.meta.url)
const CRATE = new URL('crates/agent-share-wasm-client/', REPO_ROOT)
const CRATE_DIR = fileURLToPath(CRATE)

const TARGET = 'wasm32-unknown-unknown'
const ARTIFACT = `target/${TARGET}/release/agent_share_wasm_client.wasm`

/**
 * Where each wasm-bindgen target lands, imported rather than spelled here:
 * `wasm-asset.ts` reads the browser binary back out of the same directory, and
 * the two must not be able to drift. Absolute, because the commands below run
 * from the crate's directory.
 */
const OUT_DIRS = { web: GLUE_DIR, nodejs: NODE_GLUE_DIR } as const

function fail(message: string): never {
  console.error(message)
  process.exit(1)
}

/**
 * The environment cargo should see, with the caller's own cargo run scrubbed
 * out of it.
 *
 * `cargo task web-wasm` reaches this file through `cargo run`, which exports
 * `CARGO_*`, `RUSTUP_TOOLCHAIN` and a dynamic-linker path into the child. Cargo
 * fingerprints build scripts against their environment, so the same build
 * invoked the two ways looked like two different builds and recompiled ring and
 * the whole iroh stack — twenty seconds, every time anyone alternated. Dropping
 * `RUSTFLAGS` matters for a second reason: cargo picks one flag source rather
 * than merging them, so an inherited value would silently replace the
 * `getrandom_backend` cfg that `.cargo/config.toml` sets for wasm32.
 */
function cargoEnv(extra: Record<string, string> = {}): Record<string, string> {
  const env: Record<string, string> = {}
  for (const [key, value] of Object.entries(process.env)) {
    if (value === undefined) continue
    if (key !== 'CARGO_HOME' && key.startsWith('CARGO')) continue
    if (
      [
        'RUSTUP_TOOLCHAIN',
        'RUSTC',
        'RUSTC_WRAPPER',
        'RUSTC_WORKSPACE_WRAPPER',
        'RUSTDOC',
        'RUSTFLAGS',
        'LD_LIBRARY_PATH',
        'DYLD_FALLBACK_LIBRARY_PATH',
      ].includes(key)
    ) {
      continue
    }
    env[key] = value
  }
  return { ...env, ...extra }
}

/**
 * A clang that can emit `wasm32`, or `null`.
 *
 * Apple clang ships no wasm backend, so `ring`'s C core fails to build with the
 * default `cc` on macOS; Homebrew LLVM does have one. Mirrors `wasm_clang` in
 * `tasks/src/ci.rs`, which picks the compiler for the same crate's CI checks.
 */
async function wasmClang(): Promise<string | null> {
  for (const candidate of [
    '/opt/homebrew/opt/llvm/bin/clang',
    '/usr/local/opt/llvm/bin/clang',
  ]) {
    if (await Bun.file(candidate).exists()) return candidate
  }
  const version = await $`clang --version`.nothrow().quiet().text()
  return version.includes('Apple clang') ? null : 'clang'
}

/** The `wasm-bindgen` version the crate actually links against. */
async function lockedBindgenVersion(): Promise<string> {
  const lock = await Bun.file(new URL('Cargo.lock', CRATE)).text()
  const match = lock.match(/name = "wasm-bindgen"\nversion = "([^"]+)"/)
  if (!match?.[1]) fail('no wasm-bindgen entry in the wasm client Cargo.lock')
  return match[1]
}

/** What to install when the CLI is absent or too far from the crate. */
async function installHint(): Promise<string> {
  return `cargo install wasm-bindgen-cli --version ${await lockedBindgenVersion()} --locked`
}

async function ensurePrereqs(): Promise<void> {
  const installed = await $`rustup target list --installed`
    .cwd(CRATE_DIR)
    .nothrow()
    .quiet()
    .text()
  if (!installed.split('\n').includes(TARGET)) {
    fail(`the ${TARGET} target is missing — run \`rustup target add ${TARGET}\``)
  }

  if ((await $`wasm-bindgen --version`.nothrow().quiet()).exitCode !== 0) {
    fail(`the wasm-bindgen CLI is missing — run \`${await installHint()}\``)
  }
}

/** Build the binary and both glue layers. */
export async function buildWasm(): Promise<void> {
  await ensurePrereqs()

  const clang = await wasmClang()
  if (!clang && process.platform === 'darwin') {
    console.warn('  no wasm-capable clang found — if ring fails, `brew install llvm`')
  }

  console.log(`  building agent-share-wasm-client (${TARGET}, release)`)
  await $`cargo build --release --target ${TARGET}`
    .cwd(CRATE_DIR)
    .env(cargoEnv(clang ? { CC: clang, [`CC_${TARGET.replaceAll('-', '_')}`]: clang } : {}))

  // `--target web` for the browser (ES modules, fetch-based load) and
  // `--target nodejs` for the CLI (CommonJS, fs-based). The same binary either
  // way; only the glue differs, so they cannot drift. Concurrent because they
  // share nothing but a read-only input, and each run is ~0.3 s.
  const runs = await Promise.all(
    (['web', 'nodejs'] as const).map(async (target) => {
      const outDir = fileURLToPath(OUT_DIRS[target])
      const bindgen = await $`wasm-bindgen --target ${target} --out-dir ${outDir} ${ARTIFACT}`
        .cwd(CRATE_DIR)
        .nothrow()
        .quiet()
      return { outDir, bindgen }
    }),
  )

  for (const { outDir, bindgen } of runs) {
    // wasm-bindgen checks its own schema against the binary's and says so
    // exactly, which is why nothing here compares version numbers first — a CLI
    // a patch ahead of the crate is usually fine, and guessing otherwise fails
    // a build that would have worked. The install line is what its message is
    // missing.
    if (bindgen.exitCode !== 0) {
      fail(`${bindgen.stderr.toString().trim()}\n\ntry \`${await installHint()}\``)
    }
    console.log(`  bindgen ${relative(fileURLToPath(REPO_ROOT), outDir)}`)
  }

  // The nodejs glue is CommonJS — `require`, `__dirname` — and Node resolves a
  // `.js` file's module system from the nearest package.json upwards. The
  // nearest one is now `agent-share-wasm`'s, which says `"type": "module"`, so
  // without this marker Node parses the glue as ESM and its
  // `${__dirname}/…_bg.wasm` read resolves against the wrong base — surfacing
  // through the CLI's catch as "the wasm client is missing", which sends you off
  // to rebuild a binary that is already there. That happened once when the
  // workspace root moved; the file sitting inside an ESM package now makes it
  // certain rather than incidental.
  await Bun.write(
    new URL('package.json', OUT_DIRS.nodejs),
    `${JSON.stringify({ type: 'commonjs' }, null, 2)}\n`,
  )
}

if (import.meta.main) await buildWasm()
