/**
 * Production bundle for the browser app + lab.
 *
 * Bun's HTML bundler inlines the wasm-bindgen JS glue but leaves the binary
 * as `new URL("…_bg.wasm", import.meta.url)` — copy it next to the chunks so
 * a static host serving `dist/` can resolve it.
 *
 * The binary is written under a content-addressed name so a CDN or browser
 * cannot serve yesterday's build under today's URL. See
 * `scripts/wasm-asset.ts`.
 */

import { wasmAsset, writeWasmPath } from './scripts/wasm-asset.ts'

await Bun.$`rm -rf dist`

// Before the bundle: `src/wasm.ts` imports the generated path, so it has to be
// correct on disk by the time Bun reads the entrypoints.
const asset = await wasmAsset()
await writeWasmPath(asset)

const result = await Bun.build({
  entrypoints: ['./index.html', './lab/index.html'],
  outdir: './dist',
  minify: true,
  target: 'browser',
})

if (!result.success) {
  for (const log of result.logs) console.error(log)
  process.exit(1)
}

await Bun.write(`./dist/${asset.name}`, asset.bytes)

for (const output of result.outputs) {
  console.log(`  ${output.path}`)
}
console.log(`  dist/${asset.name}`)
