import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { GlobalRegistrator } from '@happy-dom/global-registrator'
import { mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { pathToFileURL } from 'node:url'

import { createFetch } from './serve.ts'

// The shape `scripts/build.ts` writes: the site's static export at the root,
// the webapp under app/, and the worker and wasm left at the root.
const FIXTURE: Record<string, string> = {
  'index.html': 'landing',
  '404.html': 'site not found',
  'docs/index.html': 'docs home',
  'docs/getting-started/index.html': 'getting started',
  'app/index.html': 'app shell',
  'app/lab/index.html': 'lab',
  'app/chunk-abcdef12.js': 'chunk',
  'index.txt': 'landing rsc',
  'sw.js': 'worker',
  'favicon.svg': 'icon',
  'wasm/abcdef123456.bin': 'wasm',
  'wasm/abcdef123456.bin.br': 'wasm br',
  'wasm/abcdef123456.bin.gz': 'wasm gz',
  '_next/static/chunks/0123456789abcdef.js': 'next chunk',
  '_pagefind/pagefind.js': 'pagefind',
  '_pagefind/fragment/en_c6ec1cf.pf_fragment': 'fragment',
  '_pagefind/pagefind.en_a0f1eb9a69.pf_meta': 'meta',
}

let dir: string
let fetch: (req: Request) => Promise<Response>

// The preload swaps in happy-dom's `Response`, which reads a `Bun.file` body as
// the string "[object Blob]". A server needs Bun's own, so this suite puts the
// natives back for its duration. The stub keeps the preload's per-test
// `happyDOM.setURL` from throwing while happy-dom is out.
beforeAll(async () => {
  await GlobalRegistrator.unregister()
  Object.assign(globalThis, { happyDOM: { setURL() {} } })
  dir = await mkdtemp(join(tmpdir(), 'serve-test-'))
  for (const [path, body] of Object.entries(FIXTURE)) await Bun.write(join(dir, path), body)
  fetch = createFetch(pathToFileURL(`${dir}/`))
})

afterAll(async () => {
  await rm(dir, { recursive: true, force: true })
  GlobalRegistrator.register()
})

const get = (path: string) => fetch(new Request(`http://localhost${path}`))

async function expectPage(path: string, body: string, status = 200) {
  const res = await get(path)
  expect({ path, status: res.status, body: await res.text() }).toEqual({ path, status, body })
}

describe('createFetch', () => {
  test('the root is the landing page', () => expectPage('/', 'landing'))

  test('docs resolve with and without a trailing slash', async () => {
    await expectPage('/docs', 'docs home')
    await expectPage('/docs/', 'docs home')
    await expectPage('/docs/getting-started', 'getting started')
    await expectPage('/docs/getting-started/', 'getting started')
  })

  test('the webapp answers every route under /app', async () => {
    await expectPage('/app', 'app shell')
    await expectPage('/app/', 'app shell')
    await expectPage('/app/files/T', 'app shell')
    await expectPage('/app/info/T', 'app shell')
    await expectPage('/app/preview/T/note.txt', 'app shell')
  })

  test('the lab sits under /app', () => expectPage('/app/lab', 'lab'))

  test('share routes outside /app are gone', async () => {
    await expectPage('/files/T', 'site not found', 404)
    await expectPage('/preview/T/note.txt', 'site not found', 404)
  })

  test('an unknown page gets the site 404', () => expectPage('/nope', 'site not found', 404))

  test('a missing asset under /app is a 404, not the shell', async () => {
    expect((await get('/app/chunk-missing0.js')).status).toBe(404)
  })

  test('the worker and wasm stay at the root', async () => {
    await expectPage('/sw.js', 'worker')
    await expectPage('/wasm/abcdef123456.bin', 'wasm')
    await expectPage('/app/chunk-abcdef12.js', 'chunk')
  })
})

const IMMUTABLE = 'public, max-age=31536000, immutable'
const EDGE = 'max-age=300, stale-while-revalidate=86400'

async function caching(path: string, headers: Record<string, string> = {}) {
  const res = await fetch(new Request(`http://localhost${path}`, { headers }))
  return {
    path,
    browser: res.headers.get('cache-control'),
    edge: res.headers.get('cdn-cache-control'),
  }
}

describe('caching', () => {
  test('every hashed name is immutable', async () => {
    for (const path of [
      '/app/chunk-abcdef12.js',
      '/wasm/abcdef123456.bin',
      '/_next/static/chunks/0123456789abcdef.js',
      '/_pagefind/fragment/en_c6ec1cf.pf_fragment',
      '/_pagefind/pagefind.en_a0f1eb9a69.pf_meta',
    ]) {
      expect(await caching(path)).toEqual({ path, browser: IMMUTABLE, edge: null })
    }
  })

  test('a fixed name the edge can hold gets a short edge TTL', async () => {
    for (const path of ['/sw.js', '/favicon.svg', '/_pagefind/pagefind.js']) {
      expect(await caching(path)).toEqual({ path, browser: 'no-cache', edge: EDGE })
    }
  })

  test('pages get no edge TTL, so a deploy never leaves one pointing at deleted chunks', async () => {
    for (const path of ['/', '/index.txt', '/docs/', '/app', '/app/files/T', '/app/lab']) {
      expect(await caching(path)).toEqual({ path, browser: 'no-cache', edge: null })
    }
  })
})

describe('revalidation', () => {
  async function revalidate(path: string, headers: Record<string, string> = {}) {
    const first = await fetch(new Request(`http://localhost${path}`, { headers }))
    const etag = first.headers.get('etag')
    const again = await fetch(
      new Request(`http://localhost${path}`, {
        headers: { ...headers, 'if-none-match': etag ?? '' },
      }),
    )
    return { etag, status: again.status, body: await again.text() }
  }

  test('a page answers 304 to its own weak etag', async () => {
    for (const path of ['/', '/docs/', '/app/files/T', '/sw.js']) {
      const { etag, status, body } = await revalidate(path)
      expect({ path, weak: etag?.startsWith('W/"'), status, body }).toEqual({
        path,
        weak: true,
        status: 304,
        body: '',
      })
    }
  })

  test('a 304 keeps the caching headers', async () => {
    const first = await get('/sw.js')
    const res = await fetch(
      new Request('http://localhost/sw.js', {
        headers: { 'if-none-match': first.headers.get('etag')! },
      }),
    )
    expect(res.headers.get('cdn-cache-control')).toBe(EDGE)
    expect(res.headers.get('etag')).toBe(first.headers.get('etag'))
  })

  test('a stale etag, a list, and a wildcard', async () => {
    const etag = (await get('/')).headers.get('etag')!
    const status = async (inm: string) =>
      (await fetch(new Request('http://localhost/', { headers: { 'if-none-match': inm } })))
        .status
    expect(await status('W/"stale"')).toBe(200)
    expect(await status(`W/"stale", ${etag}`)).toBe(304)
    expect(await status('*')).toBe(304)
  })

  test('each wasm encoding has its own etag', async () => {
    const etag = async (enc: string) =>
      (await fetch(new Request('http://localhost/wasm/abcdef123456.bin', {
        headers: { 'accept-encoding': enc },
      }))).headers.get('etag')
    const tags = [await etag('br'), await etag('gzip'), await etag('identity')]
    expect(new Set(tags).size).toBe(3)
    expect((await revalidate('/wasm/abcdef123456.bin', { 'accept-encoding': 'br' })).status).toBe(304)
  })

  test('a 404 carries no etag', async () => {
    expect((await get('/nope')).headers.get('etag')).toBeNull()
  })
})
