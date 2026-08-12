/**
 * Hand out a built `dist/` over HTTP — the rules a static host has to follow,
 * stated once for every caller that serves this app.
 *
 * Two callers: `start.ts` locally, and the container image, where this file is
 * the entrypoint. The rules are not obvious and each one is here because of a
 * specific failure — the `/service-worker/` 404, the SPA fallback that has to
 * ignore extensions on share routes, the wasm's content type and its
 * precompressed siblings. A second copy of any of them is a copy that will be
 * wrong somewhere, which is why an nginx config is not what runs in front of
 * this.
 *
 * Nothing here reads the crate's `dist/`, deliberately: `wasmAsset()` needs
 * `crates/agent-share-wasm-client/dist/web/`, which the runtime image does not
 * have. That absence is the whole reason this is separate from `start.ts`.
 *
 * `PORT` picks the port. `DIST_DIR` overrides the directory served and is only
 * needed if this file is ever separated from its `dist/` sibling — the default
 * resolves against *this module*, so it is `web/dist/` from a checkout and
 * `/app/dist/` from the image with nothing to configure.
 */

import { pathToFileURL } from 'node:url'

import { STREAM_PREFIX } from '../src/service-worker/protocol.ts'

/** The directory served. See the header. */
export const DIST_ROOT = process.env.DIST_DIR
  ? pathToFileURL(`${process.env.DIST_DIR}/`)
  : new URL('../dist/', import.meta.url)

/**
 * Bun reads `PORT` on its own when `port` is omitted, but only as an
 * undocumented default — spelling it out is what makes it discoverable, and it
 * is what lets a local run sit beside `bun run dev` instead of losing a coin
 * flip for 3000 and exiting `EADDRINUSE`.
 */
export const PORT = Number(process.env.PORT ?? 3000)

export function distFile(pathname: string, root: URL = DIST_ROOT) {
  return Bun.file(new URL(`.${pathname}`, root))
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

/**
 * Bun names every chunk it emits `<name>-<hash>.<ext>`, so a cached copy is
 * unreachable once the contents change — the trade the wasm's
 * content-addressed name makes, and the only one that makes a far-future
 * expiry safe.
 */
const HASHED = /-[a-z0-9]{8,}\.[a-z0-9]+$/

const IMMUTABLE = { 'cache-control': 'public, max-age=31536000, immutable' }

/**
 * `sw.js` and the SPA shell are the only files in `dist/` whose names stay put
 * when their contents move: the worker cannot be hashed (the browser refetches
 * that exact URL to decide whether an update exists, so a hashed name registers
 * a second worker per build instead of updating the installed one) and the
 * shell is what every route resolves to, carrying the pointer to the current
 * chunk graph. Cached, either one keeps an old build answering — the failure
 * `wasm-asset.ts` documents, which surfaces as a decode error somewhere else
 * entirely. A local run gets away without this because a dev reloads hard; a
 * real host in front of the image would not.
 */
const NO_CACHE = { 'cache-control': 'no-cache' }

export function createFetch(root: URL = DIST_ROOT) {
  const file = (pathname: string) => distFile(pathname, root)
  const shell = file('/index.html')

  return async function fetch(req: Request): Promise<Response> {
    const { pathname } = new URL(req.url)

    if (pathname === '/lab' || pathname === '/lab/') {
      return new Response(file('/lab/index.html'), { headers: NO_CACHE })
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

    // Root scope from a root-served script is the default, but stating it keeps
    // the answer true if the script ever moves, as `dev.ts` does.
    if (pathname === '/sw.js') {
      return new Response(file('/sw.js'), {
        headers: { ...NO_CACHE, 'service-worker-allowed': '/' },
      })
    }

    // The wasm negotiates its precompressed siblings, like a static host
    // with precompressed-asset support would. `content-type` stays
    // `application/wasm` so `instantiateStreaming` engages.
    if (pathname.startsWith('/wasm/') && (await file(pathname).exists())) {
      const accepted = req.headers.get('accept-encoding') ?? ''
      const headers: Record<string, string> = {
        'content-type': 'application/wasm',
        ...IMMUTABLE,
        vary: 'accept-encoding',
      }
      for (const [token, suffix] of [
        ['br', '.br'],
        ['gzip', '.gz'],
      ] as const) {
        if (!new RegExp(`\\b${token}\\b`).test(accepted)) continue
        const compressed = file(pathname + suffix)
        if (!(await compressed.exists())) continue
        headers['content-encoding'] = token
        return new Response(compressed, { headers })
      }
      return new Response(file(pathname), { headers })
    }

    const candidate = pathname === '/' ? '/index.html' : pathname
    const target = file(candidate)
    if (await target.exists()) {
      return new Response(target, {
        headers: HASHED.test(candidate) ? IMMUTABLE : NO_CACHE,
      })
    }

    if (!looksLikeAsset(pathname)) {
      return new Response(shell, { headers: NO_CACHE })
    }

    return new Response('Not Found', { status: 404 })
  }
}

export function serve(root: URL = DIST_ROOT) {
  return Bun.serve({ port: PORT, fetch: createFetch(root) })
}

// The container's entrypoint; a no-op when `start.ts` imports this module.
if (import.meta.main) {
  const server = serve()
  console.log(`serve ${server.url}`)
}
