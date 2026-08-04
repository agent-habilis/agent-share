/**
 * Dev server: HTML multipage (`/` + `/lab`) plus the wasm binary at a
 * content-addressed HTTP path. The crate's glue defaults to `file://` for that
 * binary; callers pass the hashed path instead (see `src/wasm.ts`).
 *
 * The hash is what stops a rebuilt binary being shadowed by a cached one —
 * Bun's route table captures the file at server start, so a fixed path would
 * keep serving the copy this process began with. See `scripts/wasm-asset.ts`.
 *
 * `/*` is the SPA catch-all so `/files/<ticket>` and `/info/<ticket>` hit the
 * app; `/lab` and the wasm path are more specific and win first.
 */

import index from './index.html'
import lab from './lab/index.html'
import { wasmAsset, writeWasmPath } from './scripts/wasm-asset.ts'

const asset = await wasmAsset()
await writeWasmPath(asset)

const server = Bun.serve({
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
    [asset.path]: new Response(asset.bytes, {
      headers: {
        'content-type': 'application/wasm',
        // Safe to cache hard: the URL changes when the bytes do.
        'cache-control': 'public, max-age=31536000, immutable',
      },
    }),
    '/*': index,
  },
  development: {
    hmr: true,
    console: true,
  },
})

console.log(`dev ${server.url}`)
console.log(`  wasm ${asset.name}`)
