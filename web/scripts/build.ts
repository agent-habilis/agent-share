/**
 * Production bundle for the browser app + lab.
 *
 * Bun's HTML bundler inlines the wasm-bindgen JS glue but leaves the binary
 * as `new URL("…_bg.wasm", import.meta.url)` — copy it next to the chunks so
 * a static host serving `dist/` can resolve it.
 *
 * The binary is written under a content-addressed name so a CDN or browser
 * cannot serve yesterday's build under today's URL. See `wasm-asset.ts`.
 */

import { brotli, syncGlue, wasmAsset, writeWasmPath } from './wasm-asset.ts'

await Bun.$`rm -rf dist`

// Before the bundle: `src/wasm/index.ts` imports the generated path and the
// glue mirror, so both have to be correct on disk by the time Bun reads the
// entrypoints.
const asset = await wasmAsset()
await writeWasmPath(asset)
await syncGlue()

const result = await Bun.build({
  entrypoints: ['./src/index.html', './src/lab/index.html'],
  outdir: './dist',
  minify: true,
  target: 'browser',
})

if (!result.success) {
  for (const log of result.logs) console.error(log)
  process.exit(1)
}

// The service worker is its own bundle, at `dist/sw.js`, and deliberately **not**
// content-addressed like the wasm beside it. A worker is identified by its URL:
// the browser refetches that exact path to decide whether an update exists, so a
// hashed name would register a second worker per build instead of updating the
// one already installed. It also has to sit at the root — a worker's default
// scope is its own directory, and only a root-served script controls
// `/service-worker/…`.
const sw = await Bun.build({
  entrypoints: ['./src/service-worker/index.ts'],
  outdir: './dist',
  naming: 'sw.js',
  minify: true,
  target: 'browser',
})

if (!sw.success) {
  for (const log of sw.logs) console.error(log)
  process.exit(1)
}

// `asset.path` rather than `asset.name`: the URL carries a `/wasm/` directory,
// and `dist/` has to mirror it or a static host answers the app's fetch with
// whatever its own not-found rule says — for an SPA, `index.html`.
await Bun.write(`./dist${asset.path}`, asset.bytes)
// Precompressed siblings for hosts (and `preview.ts`) that can serve them —
// the binary is the connect path's largest download by an order of magnitude.
await Bun.write(`./dist${asset.path}.br`, await brotli(asset.bytes))
await Bun.write(`./dist${asset.path}.gz`, Bun.gzipSync(asset.bytes, { level: 9 }))

// No `<link rel="preload">` for the binary, deliberately: Safari does not
// match an `as="fetch"` preload to the glue's later `fetch()` (measured —
// two resource-timing entries, `link` then `fetch`), so on a cold cache it
// downloads the binary twice. The eager `loadWasm()` in `src/main.tsx`
// starts the real fetch within ~25 ms of where the preload would, in every
// browser, with nothing to mismatch.
//
// Bun writes chunk URLs relative to the page, but the SPA shell is served
// for every route — under `/files/<ticket>` a `./chunk-…` resolves to
// `/files/chunk-…` and 404s, which is a blank page. Absolute URLs cost
// nothing and hold on any route depth. Lab keeps its `../` (it is only ever
// served at `/lab`).
{
  const page = './dist/index.html'
  const html = await Bun.file(page).text()
  await Bun.write(
    page,
    html.replaceAll('src="./', 'src="/').replaceAll('href="./', 'href="/'),
  )
}

for (const output of result.outputs) {
  console.log(`  ${output.path}`)
}
console.log(`  dist${asset.path} (+.br, +.gz)`)
