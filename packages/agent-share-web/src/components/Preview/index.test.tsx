/**
 * The preview pane buffers a whole file before it can draw anything, so every
 * state here is a state the user actually sits in: loading, loaded, refused,
 * failed. These pin what each one says, and that leaving cleans up after
 * itself — an object URL nobody revokes holds the whole file in memory for as
 * long as the tab lives.
 */

import { test, expect, beforeEach, afterEach } from 'bun:test'
import { render, flushSync } from 'visage-dom'
import type { Root } from 'visage-dom'

import { Preview, type PreviewClient } from './index.tsx'
import type { FileNode } from '../../lib/tree.ts'

let host: HTMLElement
let root: Root | null = null
let created: { url: string; type: string; size: number }[] = []
let revoked: string[] = []

const originalCreate = URL.createObjectURL
const originalRevoke = URL.revokeObjectURL

beforeEach(() => {
  document.body.innerHTML = ''
  host = document.createElement('div')
  document.body.appendChild(host)
  created = []
  revoked = []
  // happy-dom does not implement either half of the object-URL store, and the
  // test wants to see the Blob's type anyway — that is what makes media play.
  URL.createObjectURL = (blob: Blob) => {
    const url = `blob:test/${created.length}`
    created.push({ url, type: blob.type, size: blob.size })
    return url
  }
  URL.revokeObjectURL = (url: string) => {
    revoked.push(url)
  }
})

afterEach(() => {
  root?.unmount()
  root = null
  URL.createObjectURL = originalCreate
  URL.revokeObjectURL = originalRevoke
})

/** `mtime` is 2026-08-08T00:00:00Z, in the seconds the manifest speaks. */
function file(name: string, size: number): FileNode {
  return { kind: 'file', name, path: name, index: 0, size, mtime: 1_786_147_200 }
}

/** Serves `bytes` in 4-byte windows, so more than one read is needed. */
function reader(bytes: Uint8Array, source_is_origin = true): PreviewClient {
  return {
    source_is_origin,
    async read(_index, offset, len) {
      const start = Number(offset)
      return bytes.slice(start, start + Math.min(len, 4))
    },
  }
}

function failing(message: string): PreviewClient {
  return {
    source_is_origin: true,
    async read() {
      throw new Error(message)
    },
  }
}

/** Let the pending reads and stream pulls settle, then repaint. */
async function settle(): Promise<void> {
  for (let i = 0; i < 20; i++) await new Promise((resolve) => setTimeout(resolve, 0))
  flushSync()
}

const bytes = (text: string) => new TextEncoder().encode(text)

let closes = 0

function mount(node: FileNode | undefined, client: PreviewClient): void {
  closes = 0
  root = render(
    Preview({
      node,
      client,
      onClose: () => {
        closes += 1
      },
    }),
    host,
  )
}

/** Dispatch at the focused element, the way a real key press arrives. */
function pressEscape(): boolean {
  const target = document.activeElement ?? window
  const event = new KeyboardEvent('keydown', {
    key: 'Escape',
    cancelable: true,
    bubbles: true,
  })
  target.dispatchEvent(event)
  return event.defaultPrevented
}

test('a text file renders its contents, selectable', async () => {
  const body = 'hello from the share\nsecond line'
  mount(file('note.txt', bytes(body).length), reader(bytes(body)))
  await settle()

  const pane = host.querySelector('.selectable')
  expect(pane?.textContent).toBe(body)
})

test('markdown renders as its source, not as HTML', async () => {
  const body = '# Heading\n\n**bold**'
  mount(file('README.md', bytes(body).length), reader(bytes(body)))
  await settle()

  expect(host.querySelector('.selectable')?.textContent).toBe(body)
  expect(host.querySelector('h1')).toBeNull()
  expect(host.querySelector('strong')).toBeNull()
})

test('an image becomes an object URL carrying its content type', async () => {
  const data = bytes('not really a png, but the bytes do not decide')
  mount(file('photo.png', data.length), reader(data))
  await settle()

  expect(created).toHaveLength(1)
  expect(created[0]?.type).toBe('image/png')
  expect(created[0]?.size).toBe(data.length)

  const img = host.querySelector('img')
  expect(img?.getAttribute('src')).toBe(created[0]?.url)
  expect(img?.getAttribute('alt')).toBe('photo.png')
})

test('a video gets a media element on the same URL', async () => {
  const data = bytes('mp4-ish')
  mount(file('clip.mp4', data.length), reader(data))
  await settle()

  const video = host.querySelector('video')
  expect(video).not.toBeNull()
  expect(video?.getAttribute('src')).toBe(created[0]?.url)
  expect(created[0]?.type).toBe('video/mp4')
})

test('leaving revokes the object URL', async () => {
  const data = bytes('some bytes')
  mount(file('photo.png', data.length), reader(data))
  await settle()
  expect(created).toHaveLength(1)

  root?.unmount()
  root = null
  expect(revoked).toEqual([created[0]!.url])
})

test('an unsupported file shows its facts and says why', async () => {
  mount(file('big.bin', 2048), reader(bytes('never read')))
  await settle()

  expect(host.textContent).toContain('no preview for this file type')
  expect(host.textContent).toContain('2.0 KB')
  expect(host.textContent).toContain('2026-08-08')
  // Nothing was fetched: the kind is decided from the name alone.
  expect(created).toHaveLength(0)
})

test('a file the URL names but the manifest lost says so', async () => {
  mount(undefined, reader(bytes('')))
  await settle()

  expect(host.textContent).toContain('nothing to preview')
})

test('a read that fails reports the reason on the page', async () => {
  mount(file('note.txt', 12), failing('the producer stopped sharing'))
  await settle()

  expect(host.textContent).toContain('the producer stopped sharing')
  expect(host.textContent).toContain('note.txt')
})

test('the pane takes focus, so key presses reach the page at all', async () => {
  const data = bytes('hello')
  mount(file('note.txt', data.length), reader(data))
  await settle()

  // Focus fell to <body> otherwise: the button that opened the preview was
  // unmounted by the navigation that opened it.
  expect(document.activeElement).not.toBe(document.body)
  expect(host.contains(document.activeElement)).toBe(true)
})

test('escape is caught whether it travels by the pane or by the window', async () => {
  const data = bytes('hello')
  mount(file('note.txt', data.length), reader(data))
  await settle()

  const event = new KeyboardEvent('keydown', {
    key: 'Escape',
    cancelable: true,
    bubbles: true,
  })
  window.dispatchEvent(event)
  expect(closes).toBe(1)

  // Handling it on both routes must not close twice.
  expect(closes).toBe(1)
})

test('escape leaves', async () => {
  const data = bytes('hello')
  mount(file('note.txt', data.length), reader(data))
  await settle()

  expect(pressEscape()).toBe(true)
  expect(closes).toBe(1)
})

test('escape leaves from the unsupported screen too', async () => {
  mount(file('big.bin', 2048), reader(bytes('')))
  await settle()

  pressEscape()
  expect(closes).toBe(1)
})

test('escape while full-screen belongs to the browser, not to us', async () => {
  const data = bytes('mp4-ish')
  mount(file('clip.mp4', data.length), reader(data))
  await settle()

  const doc = document as Document & { fullscreenElement: Element | null }
  const original = doc.fullscreenElement
  doc.fullscreenElement = host
  try {
    // Exiting full-screen and leaving the preview would both happen on one
    // press, so the video the user was watching would vanish.
    expect(pressEscape()).toBe(false)
    expect(closes).toBe(0)
  } finally {
    doc.fullscreenElement = original
  }

  expect(pressEscape()).toBe(true)
  expect(closes).toBe(1)
})

test('leaving unbinds the key, so a later escape reaches nothing', async () => {
  const data = bytes('hello')
  mount(file('note.txt', data.length), reader(data))
  await settle()

  root?.unmount()
  root = null
  pressEscape()
  expect(closes).toBe(0)
})

test('while the bytes are still coming, the pane says how far along it is', async () => {
  let release: (() => void) | null = null
  const held = new Promise<void>((resolve) => {
    release = resolve
  })
  const slow: PreviewClient = {
    source_is_origin: true,
    async read() {
      await held
      return bytes('done')
    },
  }
  mount(file('clip.mp4', 4), slow)
  await settle()

  expect(host.querySelector('[role="progressbar"]')).not.toBeNull()
  expect(host.textContent).toContain('clip.mp4')
  expect(host.querySelector('video')).toBeNull()

  release?.()
  await settle()
  expect(host.querySelector('video')).not.toBeNull()
})
