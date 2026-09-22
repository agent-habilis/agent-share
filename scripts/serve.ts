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
 * Nothing here reads the wasm build output, deliberately: `wasmAsset()` needs
 * `packages/agent-share-wasm/src/glue/`, which the runtime image does not have.
 * That absence is the whole reason this is separate from `start.ts`.
 *
 * `PORT` picks the port. `DIST_DIR` overrides the directory served and is only
 * needed if this file is ever separated from its `dist/` sibling — the default
 * resolves against *this module*, so it is the repo root's `dist/` from a
 * checkout and `/app/dist/` from the image with nothing to configure.
 */

import { pathToFileURL } from 'node:url'

import { STREAM_PREFIX } from '../packages/agent-share-web/src/lib/stream/protocol.ts'

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
const SHARE_ROUTE = /^\/app\/(files|info|preview)\//

/**
 * The webapp's mount point. Everything else in `dist/` is the site's static
 * export (landing page and docs), which has a real file for every page and its
 * own `404.html` — so only paths under here fall back to the SPA shell.
 */
const APP_ROUTE = /^\/app(\/|$)/

function looksLikeAsset(pathname: string): boolean {
  if (SHARE_ROUTE.test(pathname)) return false
  const last = pathname.split('/').pop() ?? ''
  return last.includes('.')
}

/**
 * Names that change whenever their contents do, so a cached copy is
 * unreachable once the contents change — the trade the wasm's
 * content-addressed name makes, and the only one that makes a far-future
 * expiry safe. Bun names chunks `<name>-<hash>.<ext>`; Next puts only hashed or
 * build-id-scoped files under `/_next/static/`; Pagefind names its index files
 * `<lang>_<hash>.pf_<kind>`.
 */
const HASHED = /-[a-z0-9]{8,}\.[a-z0-9]+$|^\/_next\/static\/|_[0-9a-f]{7,}\.pf_[a-z]+$/

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

/**
 * A fixed name that is not a page: `sw.js`, the favicon, Pagefind's runtime.
 * Cloudflare holds these for five minutes, which keeps the origin out of the
 * common case without a purge on deploy. The edge TTL rides `CDN-Cache-Control`
 * (RFC 9213) rather than `s-maxage` because browsers honor
 * `stale-while-revalidate` too, and in a browser it would keep an old file for
 * a day.
 */
const EDGE_SHORT = { ...NO_CACHE, 'cdn-cache-control': 'max-age=300, stale-while-revalidate=86400' }

/**
 * Pages, and the RSC payloads Next fetches beside them. Never held at the edge:
 * a page names the chunks of its own build, which a deploy deletes, so a stale
 * one renders blank.
 */
const PAGE = /\.(html|txt)$/

function cachingFor(pathname: string) {
  if (HASHED.test(pathname)) return IMMUTABLE
  if (PAGE.test(pathname)) return NO_CACHE
  return EDGE_SHORT
}

/**
 * Size and mtime, never contents: `start.ts` serves a `dist/` that a rebuild
 * rewrites under it, and a table hashed at startup would answer 304 for bytes
 * that changed. Weak, because Cloudflare weakens a strong tag whenever it
 * re-encodes a response. `variant` separates the precompressed siblings, which
 * share a URL.
 */
function etagOf(file: ReturnType<typeof Bun.file>, variant = '') {
  return `W/"${file.size.toString(36)}-${file.lastModified.toString(36)}${variant}"`
}

/** `If-None-Match` uses the weak comparison, so the `W/` prefix is ignored. */
function matches(ifNoneMatch: string | null, etag: string) {
  if (!ifNoneMatch) return false
  if (ifNoneMatch.trim() === '*') return true
  const opaque = etag.replace(/^W\//, '')
  return ifNoneMatch.split(',').some((tag) => tag.trim().replace(/^W\//, '') === opaque)
}

function fileResponse(
  req: Request,
  file: ReturnType<typeof Bun.file>,
  headers: Record<string, string>,
  variant = '',
): Response {
  const etag = etagOf(file, variant)
  const all = { ...headers, etag }
  if (matches(req.headers.get('if-none-match'), etag)) {
    return new Response(null, { status: 304, headers: all })
  }
  return new Response(file, { headers: all })
}

export function createFetch(root: URL = DIST_ROOT) {
  const file = (pathname: string) => distFile(pathname, root)
  const shell = file('/app/index.html')
  const notFound = file('/404.html')

  return async function fetch(req: Request): Promise<Response> {
    const { pathname } = new URL(req.url)

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
      return fileResponse(req, file('/sw.js'), {
        ...EDGE_SHORT,
        'service-worker-allowed': '/',
      })
    }

    // The wasm negotiates its precompressed siblings, like a static host
    // with precompressed-asset support would. `content-type` stays
    // `application/wasm` so `instantiateStreaming` engages. The file is named
    // `.bin` (see `wasm-asset.ts`) because Cloudflare picks what to cache by
    // extension alone and `.wasm` is not on its list; that also means turning
    // on its Cache Deception Armor would stop caching it, since the extension
    // and the content type disagree.
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
        return fileResponse(req, compressed, headers, `-${token}`)
      }
      return fileResponse(req, file(pathname), headers)
    }

    const candidate = pathname === '/' ? '/index.html' : pathname
    const target = file(candidate)
    if (await target.exists()) {
      return fileResponse(req, target, cachingFor(candidate))
    }

    if (!looksLikeAsset(pathname)) {
      // A directory's own index before the SPA shell, the way a static host
      // resolves one. Every page bundles to `dist/<name>/index.html` and is
      // served at `/<name>`, so this is what answers `/app/lab` and every docs
      // page — as a rule rather than by name, which is what keeps a second page
      // from needing a second branch here.
      const index = file(`${pathname.replace(/\/$/, '')}/index.html`)
      if (await index.exists()) {
        return fileResponse(req, index, NO_CACHE)
      }
      if (APP_ROUTE.test(pathname)) {
        return fileResponse(req, shell, NO_CACHE)
      }
    }

    if (await notFound.exists()) {
      return new Response(notFound, { status: 404, headers: NO_CACHE })
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
