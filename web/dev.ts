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

const WASM_SRC =
  '../crates/agent-share-wasm-client/dist/web/agent_share_wasm_client_bg.wasm'

if (!(await Bun.file(WASM_SRC).exists())) {
  console.error(
    'wasm missing — run `cargo task web-wasm` before `bun run dev`',
  )
  process.exit(1)
}

const server = Bun.serve({
  routes: {
    '/lab': lab,
    '/lab/': lab,
    /**
     * Opened per request, and never cached.
     *
     * A `Bun.file` handle captured once at module load kept serving whatever
     * the file was when the server booted, so a `cargo task web-wasm` run
     * mid-session was invisible until the server was restarted. The browser
     * then paired a stale module with freshly bundled glue and died with
     * `CompileError: … Custom section … would overflow Module's size`, which
     * names neither the cause nor the fix. Measured serving 7,382,976 bytes
     * while disk held 7,383,873.
     *
     * `no-store` for the same reason one level up: the path is fixed, so a
     * browser that cached a previous build has no way to notice a new one.
     */
    '/agent_share_wasm_client_bg.wasm': () =>
      new Response(Bun.file(WASM_SRC), {
        headers: { 'cache-control': 'no-store' },
      }),
    '/*': index,
  },
  development: {
    hmr: true,
    console: true,
  },
})

console.log(`dev ${server.url}`)
