/**
 * `bun run start`: serve a production `dist/` build. Run `bun run build` first.
 *
 * The routing lives in `serve.ts`, which is also what the container image runs,
 * so a local run and a deployed one answer identically. What is left here is
 * the pair of checks that only mean anything in a checkout: `dist/` exists, and
 * the wasm inside it is the one the current crate build produced. Both read
 * `crates/agent-share-wasm-client/dist/`, which the image does not carry — that
 * is why this is a wrapper rather than a flag.
 *
 * `PORT` picks the port, so this can run alongside `bun run dev` instead of
 * losing a coin flip for 3000 and exiting `EADDRINUSE`.
 */

import { createFetch, distFile, PORT } from './serve.ts'
import { wasmAsset } from './wasm-asset.ts'

if (!(await distFile('/index.html').exists())) {
  console.error('dist/ missing — run `bun run build` first')
  process.exit(1)
}

// The binary in `dist/` must be the one the current crate build produced.
// Serving a `dist/` built against an older wasm is exactly the failure the
// content-addressed name exists to prevent, so say so rather than serve it.
const asset = await wasmAsset()
if (!(await distFile(asset.path).exists())) {
  console.error(
    `dist/ has no ${asset.name} — the wasm changed since \`bun run build\`; rebuild`,
  )
  process.exit(1)
}

const server = Bun.serve({ port: PORT, fetch: createFetch() })

console.log(`start ${server.url}`)
