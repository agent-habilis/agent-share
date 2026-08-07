/**
 * Serve a production `dist/` build. Run `bun run build` first.
 *
 * Bun's HTML multipage server rebundles source; this only hands out the
 * already-built files so preview matches a static host. Extensionless paths
 * (share routes like `/files/<ticket>`) fall back to the SPA `index.html`.
 *
 * `PORT` picks the port, so a preview can run alongside `bun run dev` instead of
 * losing a coin flip for 3000 and exiting `EADDRINUSE`. Bun reads `PORT` on its
 * own when `port` is omitted, but only as an undocumented default — spelling it
 * out is what makes it discoverable from here.
 */

import { wasmAsset } from './scripts/wasm-asset.ts'

const ROOT = new URL('./dist/', import.meta.url)

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

function looksLikeAsset(pathname: string): boolean {
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
