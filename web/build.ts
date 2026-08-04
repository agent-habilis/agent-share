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

/**
 * Content-hash the wasm, the way Bun already hashes the JS chunks.
 *
 * Shipped at a fixed name it was the one asset a returning visitor could not
 * be given fresh: the glue got a new hashed URL every deploy while the binary
 * kept its old one, so a cached module met new glue. wasm-bindgen's import
 * list changes between builds — observed dropping a `__wbindgen_cast_*` across
 * one rebuild — so that pairing does not degrade, it fails outright on
 * `WebAssembly.instantiate`, and a pure static host has nothing to patch it
 * with. A hashed name makes the stale copy unreachable instead of unlucky.
 */
const wasmBytes = await wasmSrc.bytes()
const digest = Bun.hash(wasmBytes).toString(16).padStart(16, '0').slice(0, 8)
const wasmName = `agent_share_wasm_client_bg.${digest}.wasm`
await Bun.write(`./dist/${wasmName}`, wasmBytes)

// Point the bundled glue at the hashed name. `wasm.ts` holds the unhashed
// path as a literal, which survives minification, so this is a rewrite of the
// emitted chunks rather than a build-time define — no generated source file to
// keep in sync, and it fails loudly below if the literal ever stops appearing.
const FIXED_PATH = '/agent_share_wasm_client_bg.wasm'
let patched = 0
for (const output of result.outputs) {
  if (!output.path.endsWith('.js')) continue
  const text = await Bun.file(output.path).text()
  if (!text.includes(FIXED_PATH)) continue
  await Bun.write(output.path, text.replaceAll(FIXED_PATH, `/${wasmName}`))
  patched += 1
}
if (patched === 0) {
  console.error(
    `no bundled chunk referenced ${FIXED_PATH} — the wasm would be fetched ` +
      `from an unhashed URL and a returning visitor could get a stale one. ` +
      `Did web/src/wasm.ts stop holding the path as a literal?`,
  )
  process.exit(1)
}

for (const output of result.outputs) {
  console.log(`  ${output.path}`)
}
console.log(`  dist/${wasmName}`)
