/**
 * Dev server: HTML multipage (`/` + `/lab`) plus the wasm binary at a
 * content-addressed HTTP path. The crate's glue defaults to `file://` for that
 * binary; callers pass the hashed path instead (see `src/wasm/index.ts`).
 *
 * The hash is what stops a rebuilt binary being shadowed by a cached one — see
 * `scripts/wasm-asset.ts`.
 *
 * # The binary is read per request, not captured at start
 *
 * Bun's route table would capture a `Bun.file` at server start, and this server
 * outlives the builds it serves: `cargo task web-wasm` mid-session used to be
 * invisible to it forever, so it went on answering for a build that no longer
 * existed. `/wasm/:name` re-reads the current binary instead (cheaply — the
 * bytes are memoised against the file's `stat`), and the watcher below
 * regenerates `src/wasm/path.ts` so `--hot` rebundles the app onto the new
 * hash. A rebuild now heals itself; nobody has to remember to restart.
 *
 * `/*` is the SPA catch-all so `/files/<ticket>` and `/info/<ticket>` hit the
 * app; `/lab` and `/wasm/:name` are more specific and win first. That
 * specificity is the point of the `/wasm/` prefix — at the URL root the
 * catch-all answered a stale hash with `index.html`, which reached the browser
 * as a wasm "expected magic word" error naming the wrong problem entirely.
 *
 * `PORT` picks the port, so two of these can run at once — one per checkout, or
 * one beside `bun run preview`. Bun reads `PORT` on its own when `port` is
 * omitted, but only as an undocumented default; spelling it out is what makes it
 * discoverable from here.
 */

import { watch } from 'node:fs'
import { stat } from 'node:fs/promises'
import { basename, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'

import index from './src/index.html'
import lab from './src/lab/index.html'
import {
  syncGlue,
  tryWasmAsset,
  wasmResponse,
  withGzip,
  writeWasmPath,
  WASM_SOURCE,
  type WasmAsset,
} from './scripts/wasm-asset.ts'

const WASM_FILE = fileURLToPath(new URL(WASM_SOURCE, import.meta.url))
const WASM_NAME = basename(WASM_FILE)

/**
 * The binary as it is on disk *now*, re-hashed only when the file changes.
 *
 * Keyed on `mtimeMs:size` so the steady state is one `stat` per page load
 * rather than a 7 MB read and SHA-256, while a rebuild still invalidates on the
 * very next request.
 */
let cached: { key: string; asset: WasmAsset } | null = null

async function currentAsset(): Promise<WasmAsset | null> {
  let key: string
  try {
    const info = await stat(WASM_FILE)
    key = `${info.mtimeMs}:${info.size}`
  } catch {
    cached = null
    return null
  }
  if (cached?.key === key) return cached.asset
  const asset = await tryWasmAsset()
  // gzip, not brotli: ~2.5 MB instead of ~2 MB, but fast enough to pay on
  // every rebuild. The production build precompresses brotli.
  cached = asset ? { key, asset: withGzip(asset) } : null
  return cached?.asset ?? null
}

/**
 * Point `src/wasm/index.ts` at the current build, so `--hot` rebundles onto it.
 * Returns the build it published, if there was one.
 */
async function writeCurrentPath(): Promise<WasmAsset | null> {
  const asset = await currentAsset()
  // `writeWasmPath` is content-guarded, so this is a no-op unless the hash
  // actually moved — which matters, because writing into `src/` is what `--hot`
  // watches, and an unconditional write would rebuild in a loop.
  if (asset) await writeWasmPath(asset)
  return asset
}

// Before binding: `tasks/src/bench/browser.rs` reads the generated path back
// out the moment this server reports a URL, and `src/wasm/index.ts` imports it.
// Going through `writeCurrentPath` rather than a bare read leaves the cache
// warm, so the first page load does not hash 7 MB a second time. The glue
// mirror must be current before the first bundle for the same reason the
// path must: `src/wasm/index.ts` imports both.
await syncGlue()
const initial = await writeCurrentPath()
if (!initial) {
  // The same bail `wasmAsset()` makes, taken here because this is the one
  // caller that has already looked and wants the answer memoised.
  console.error('wasm missing — run `cargo task web-wasm` first')
  process.exit(1)
}

const server = Bun.serve({
  port: Number(process.env.PORT ?? 3000),
  routes: {
    '/lab': lab,
    '/lab/': lab,
    // The pre-hash path, answered explicitly. Without this the SPA catch-all
    // below takes it and hands back HTML, which surfaces as a wasm "expected
    // magic word" error — technically loud, but it names the wrong problem.
    '/agent_share_wasm_client_bg.wasm': new Response(
      'this build serves the wasm under a content-addressed name; rebuild the app bundle',
      { status: 404 },
    ),
    // Rebundled per request, and never cached. A worker's default scope is its
    // own directory, so it has to be served from the root to control
    // `/__stream/…` — and a stale one is the worst thing in this system to
    // debug, since it fails as a decode error somewhere else entirely.
    '/sw.js': async () => {
      const built = await Bun.build({ entrypoints: ['./src/sw/index.ts'], target: 'browser' })
      const [output] = built.outputs
      if (!output) {
        return new Response('// service worker failed to build', {
          status: 500,
          headers: { 'content-type': 'text/javascript;charset=utf-8' },
        })
      }
      return new Response(await output.text(), {
        headers: {
          'content-type': 'text/javascript;charset=utf-8',
          'cache-control': 'no-cache',
          // Root scope from a root-served script is the default, but stating it
          // keeps the answer true if the script ever moves.
          'service-worker-allowed': '/',
        },
      })
    },
    // Answered explicitly, for the same reason as the pre-hash wasm path above:
    // these URLs exist only while a worker is controlling the page, and the SPA
    // catch-all would otherwise hand `index.html` to a media element — which
    // surfaces as a codec error naming the wrong problem.
    '/__stream/*': new Response(
      'no service worker is controlling this page, so nothing can answer a stream URL',
      { status: 404, headers: { 'content-type': 'text/plain;charset=utf-8' } },
    ),
    '/wasm/:name': async (req) => {
      const current = await currentAsset()
      if (current && req.params.name === current.name) {
        return wasmResponse(current, req.headers.get('accept-encoding'))
      }
      // Never fall through to the SPA shell here. A page asking for a hash we
      // do not have is stale, and saying so is worth more than 7 MB of the
      // wrong answer or 750 bytes of HTML.
      return new Response(
        `no such wasm build: ${req.params.name}\n` +
          `current: ${current?.name ?? 'none — run `cargo task web-wasm`'}\n` +
          `this page predates the current build; hard-refresh it\n`,
        { status: 404, headers: { 'content-type': 'text/plain;charset=utf-8' } },
      )
    },
    '/*': index,
  },
  development: {
    hmr: true,
    console: true,
  },
})

// `cargo task web-wasm` replaces the binary, so watch its directory rather than
// the file — a watch on the path itself follows the old inode into the bin.
// Debounced because wasm-bindgen writes in stages, and hashing a half-written
// file would publish a path for a build that never existed.
//
// The sibling glue files are watched too — the LinkError this heals: the
// glue is bundled from the `src/wasm/glue/` mirror, and without a re-sync
// here a rebuilt binary met glue cached at server start, failing to
// instantiate on a binding only one side knew about.
try {
  let pending: ReturnType<typeof setTimeout> | null = null
  watch(dirname(WASM_FILE), (_event, filename) => {
    // `null` filename (some platforms report only that *something* changed) is
    // taken as a maybe and re-checked.
    if (filename && filename !== WASM_NAME && !filename.startsWith('agent_share_wasm_client.'))
      return
    if (pending) clearTimeout(pending)
    pending = setTimeout(() => {
      pending = null
      void Promise.all([writeCurrentPath(), syncGlue()]).then(([asset, glueMoved]) => {
        console.log(asset ? `  wasm ${asset.name}` : '  wasm missing')
        if (glueMoved) console.log('  glue re-synced into src/wasm/glue/')
      })
    }, 150)
  }).unref()
} catch {
  // No watch (missing directory, platform limit): the per-request read above
  // still serves the right bytes, a restart still picks up the new path.
  console.warn('  wasm rebuilds will not hot-reload — could not watch the build directory')
}

console.log(`dev ${server.url}`)
console.log(`  wasm ${initial.name}`)
