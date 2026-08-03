/**
 * Dev server: HTML multipage (`/` + `/lab`) plus the wasm binary at a stable
 * HTTP path. The crate's glue defaults to `file://` for that binary; callers
 * pass `/agent_share_wasm_client_bg.wasm` instead (see `src/wasm.ts`).
 *
 * `/*` is the SPA catch-all so `/files/<ticket>` and `/info/<ticket>` hit the
 * app; `/lab` and the wasm path are more specific and win first.
 */

import index from './index.html'
import lab from './lab/index.html'

const wasm = Bun.file(
  '../crates/agent-share-wasm-client/dist/web/agent_share_wasm_client_bg.wasm',
)
if (!(await wasm.exists())) {
  console.error(
    'wasm missing — run `cargo task web-wasm` before `bun run dev`',
  )
  process.exit(1)
}

const server = Bun.serve({
  routes: {
    '/lab': lab,
    '/lab/': lab,
    '/agent_share_wasm_client_bg.wasm': wasm,
    '/*': index,
  },
  development: {
    hmr: true,
    console: true,
  },
})

console.log(`dev ${server.url}`)
