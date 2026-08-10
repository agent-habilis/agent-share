import { describe, expect, test } from 'bun:test'

import { singleFileStream, zipStream } from './index.ts'
import type { FileNode } from '../tree.ts'

/**
 * The seam that makes "seed what we already fetched" true.
 *
 * Everything that moves bytes — a single download, a folder as a ZIP, a preview
 * — goes through `fileStream`, so hooking the reader once covers all three.
 * These cases pin that, and pin the two properties that keep it from ever
 * costing a transfer: it is never awaited, and it never throws.
 */

function node(index: number, size: number): FileNode {
  return { kind: 'file', name: 'a.bin', path: 'a.bin', size, mtime: 0, index }
}

interface Kept {
  index: number
  offset: bigint
  bytes: Uint8Array
}

function reader(size: number, kept: Kept[], keep?: () => Promise<void>) {
  return {
    read(_index: number, offset: bigint, len: number) {
      const start = Number(offset)
      const take = Math.min(len, size - start)
      return Promise.resolve(
        new Uint8Array(Array.from({ length: Math.max(take, 0) }, (_, at) => (start + at) % 251)),
      )
    },
    keep(index: number, offset: bigint, bytes: Uint8Array) {
      kept.push({ index, offset, bytes })
      return keep ? keep() : Promise.resolve()
    },
  }
}

async function drain(stream: ReadableStream<Uint8Array>): Promise<number> {
  return (await new Response(stream).arrayBuffer()).byteLength
}

describe('keeping what was fetched', () => {
  test('a single-file download hands every chunk back, in order and at its own offset', async () => {
    const kept: Kept[] = []
    const size = 700 * 1024
    await drain(singleFileStream(reader(size, kept), node(3, size)))

    expect(kept.length).toBeGreaterThan(1)
    expect(kept.every((entry) => entry.index === 3)).toBe(true)
    // Offsets are where the bytes came from, not where the next read starts —
    // the difference between seeding a file and seeding a shifted copy of it.
    let expected = 0n
    for (const entry of kept) {
      expect(entry.offset).toBe(expected)
      expected += BigInt(entry.bytes.length)
    }
    expect(Number(expected)).toBe(size)
  })

  test('a folder as a ZIP keeps every file under its own index', async () => {
    const kept: Kept[] = []
    const size = 300 * 1024
    const files = [node(1, size), node(4, size)]
    await drain(zipStream(reader(size, kept), files))

    const indices = new Set(kept.map((entry) => entry.index))
    expect(indices).toEqual(new Set([1, 4]))
  })

  test('a reader with no keep still streams', async () => {
    // The hook is optional, and a plain reader must not be broken by it.
    const size = 128 * 1024
    const plain = {
      read: (_index: number, offset: bigint, len: number) =>
        Promise.resolve(new Uint8Array(Math.min(len, size - Number(offset)))),
    }
    expect(await drain(singleFileStream(plain, node(0, size)))).toBe(size)
  })

  test('a keep that rejects never fails the transfer', async () => {
    // Storage can be refused outright — private mode, a full quota — and that
    // must cost seeding rather than the download the user actually asked for.
    const kept: Kept[] = []
    const size = 200 * 1024
    const stream = singleFileStream(
      reader(size, kept, () => Promise.reject(new Error('quota exceeded'))),
      node(0, size),
    )
    expect(await drain(stream)).toBe(size)
    expect(kept.length).toBeGreaterThan(0)
  })

  test('a keep that never settles never stalls the transfer', async () => {
    // Not awaited, deliberately: storing sits beside the transfer, not inside
    // it. A hung IndexedDB write must not become a hung download.
    const kept: Kept[] = []
    const size = 200 * 1024
    const stream = singleFileStream(
      reader(size, kept, () => new Promise<void>(() => undefined)),
      node(0, size),
    )
    expect(await drain(stream)).toBe(size)
  })

  test('an aborted download still keeps the chunks that landed', async () => {
    // The direct answer to partial holdings going invisible: a cancelled
    // transfer is still worth something to the swarm.
    const kept: Kept[] = []
    const size = 900 * 1024
    const abort = new AbortController()
    const stream = singleFileStream(reader(size, kept), node(0, size), undefined, abort.signal)
    const body = stream.getReader()
    await body.read()
    await body.read()
    abort.abort()
    await body.cancel().catch(() => undefined)

    expect(kept.length).toBeGreaterThan(0)
    expect(Number(kept[0]!.offset)).toBe(0)
  })
})
