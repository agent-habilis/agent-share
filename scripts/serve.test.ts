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
  'sw.js': 'worker',
  'wasm/abcdef123456.wasm': 'wasm',
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
    await expectPage('/wasm/abcdef123456.wasm', 'wasm')
    await expectPage('/app/chunk-abcdef12.js', 'chunk')
  })
})
