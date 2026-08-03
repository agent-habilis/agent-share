/**
 * Production bundle for the browser app + lab.
 *
 * Bun's HTML bundler inlines the wasm-bindgen JS glue but leaves the binary
 * as `new URL("…_bg.wasm", import.meta.url)` — copy it next to the chunks so
 * a static host serving `dist/` can resolve it.
 */

await Bun.$`rm -rf dist`

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

const wasmSrc = Bun.file(
  '../crates/agent-share-wasm-client/dist/web/agent_share_wasm_client_bg.wasm',
)
if (!(await wasmSrc.exists())) {
  console.error(
    'wasm missing — run `cargo task web-wasm` before `bun run build`',
  )
  process.exit(1)
}
await Bun.write('./dist/agent_share_wasm_client_bg.wasm', wasmSrc)

for (const output of result.outputs) {
  console.log(`  ${output.path}`)
}
console.log('  dist/agent_share_wasm_client_bg.wasm')
