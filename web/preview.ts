/**
 * Serve a production `dist/` build. Run `bun run build` first.
 *
 * Bun's HTML multipage server rebundles source; this only hands out the
 * already-built files so preview matches a static host. Extensionless paths
 * (share routes like `/files/<ticket>`) fall back to the SPA `index.html`.
 */

const ROOT = new URL('./dist/', import.meta.url)

function distFile(pathname: string) {
  return Bun.file(new URL(`.${pathname}`, ROOT))
}

const distIndex = distFile('/index.html')
if (!(await distIndex.exists())) {
  console.error('dist/ missing — run `bun run build` first')
  process.exit(1)
}

function looksLikeAsset(pathname: string): boolean {
  const last = pathname.split('/').pop() ?? ''
  return last.includes('.')
}

const server = Bun.serve({
  async fetch(req) {
    const { pathname } = new URL(req.url)

    if (pathname === '/lab' || pathname === '/lab/') {
      return new Response(distFile('/lab/index.html'))
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
