/**
 * Serve a production `dist/` build. Run `bun run build` first.
 *
 * Bun's HTML multipage server rebundles source; this only hands out the
 * already-built files so preview matches a static host. Extensionless paths,
 * and share routes however they are spelled, fall back to the SPA
 * `index.html` — see `looksLikeAsset`.
 *
 * `PORT` picks the port, so a preview can run alongside `bun run dev` instead of
 * losing a coin flip for 3000 and exiting `EADDRINUSE`. Bun reads `PORT` on its
 * own when `port` is omitted, but only as an undocumented default — spelling it
 * out is what makes it discoverable from here.
 */

import { wasmAsset } from './wasm-asset.ts'
import { STREAM_PREFIX } from '../src/service-worker/protocol.ts'

const ROOT = new URL('../dist/', import.meta.url)

function distFile(pathname: string) {
  return Bun.file(new URL(`.${pathname}`, ROOT))
}

const distIndex = distFile('/index.html')
if (!(await distIndex.exists())) {
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

/**
 * Share routes, which always get the SPA shell however they are spelled.
 *
 * `/preview/<ticket>/note.txt` ends in a dot and would otherwise read as a
 * missing asset and 404 — the preview view carries its file in the path, so
 * the extension-shaped tail is the normal case rather than the odd one.
 */
const SHARE_ROUTE = /^\/(files|info|preview)\//

function looksLikeAsset(pathname: string): boolean {
  if (SHARE_ROUTE.test(pathname)) return false
  const last = pathname.split('/').pop() ?? ''
  return last.includes('.')
}

const server = Bun.serve({
  port: Number(process.env.PORT ?? 3000),
  async fetch(req) {
    const { pathname } = new URL(req.url)

    if (pathname === '/lab' || pathname === '/lab/') {
      return new Response(distFile('/lab/index.html'))
    }

    // Answered explicitly, matching the dev server. These URLs exist only while
    // a service worker is controlling the page, so reaching the network means
    // there is none — and the SPA fallback below would hand `index.html` to a
    // media element, which surfaces as a codec error naming the wrong problem.
    // `looksLikeAsset` happens to 404 the common case already, but only because
    // the name carries an extension; a shared file without one would take the
    // shell.
    if (pathname.startsWith(`${STREAM_PREFIX}/`)) {
      return new Response(
        'no service worker is controlling this page, so nothing can answer a stream URL',
        { status: 404, headers: { 'content-type': 'text/plain;charset=utf-8' } },
      )
    }

    // The wasm negotiates its precompressed siblings, like a static host
    // with precompressed-asset support would. `content-type` stays
    // `application/wasm` so `instantiateStreaming` engages.
    if (pathname.startsWith('/wasm/') && (await distFile(pathname).exists())) {
      const accepted = req.headers.get('accept-encoding') ?? ''
      const headers: Record<string, string> = {
        'content-type': 'application/wasm',
        'cache-control': 'public, max-age=31536000, immutable',
        vary: 'accept-encoding',
      }
      for (const [token, suffix] of [
        ['br', '.br'],
        ['gzip', '.gz'],
      ] as const) {
        if (!new RegExp(`\\b${token}\\b`).test(accepted)) continue
        const compressed = distFile(pathname + suffix)
        if (!(await compressed.exists())) continue
        headers['content-encoding'] = token
        return new Response(compressed, { headers })
      }
      return new Response(distFile(pathname), { headers })
    }

    const candidate = pathname === '/' ? '/index.html' : pathname
    const file = distFile(candidate)
    if (await file.exists()) {
      return new Response(file)
    }

    if (!looksLikeAsset(pathname)) {
      return new Response(distIndex)
    }

    return new Response('Not Found', { status: 404 })
  },
})

console.log(`preview ${server.url}`)
