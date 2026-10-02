import { describe, expect, test } from 'bun:test'

import { answerRead, type Reply } from './index.ts'
import { respond, type StreamEntry } from './range.ts'
import type { ReadMessage, ReadReply } from './protocol.ts'

/**
 * The streaming half of "seed what we already fetched".
 *
 * Video and audio are the only preview kinds that go through the service
 * worker, and that path used to read the whole file and keep none of it — so a
 * tab that had just watched a video advertised nothing of it. These cases pin
 * the keep, and pin the property that keeps it from ever costing playback: it
 * runs after the reply, and it cannot turn a good read into an error.
 *
 * `answerRead` is exercised rather than `openStream` because everything else in
 * that module needs a `ServiceWorkerGlobalScope`, which happy-dom does not have.
 */

function ask(over: Partial<ReadMessage> = {}): ReadMessage {
  return { type: 'read', reqId: 7, index: 3, offset: 65536, len: 1024, ...over }
}

interface Kept {
  index: number
  offset: bigint
  bytes: Uint8Array
}

function reader(kept: Kept[], keep?: () => Promise<void>) {
  return {
    read: (_index: number, offset: bigint, len: number) =>
      Promise.resolve(
        new Uint8Array(Array.from({ length: len }, (_, at) => (Number(offset) + at) % 251)),
      ),
    keep(index: number, offset: bigint, bytes: Uint8Array) {
      kept.push({ index, offset, bytes })
      return keep ? keep() : Promise.resolve()
    },
  }
}

function collector(): { sent: ReadReply[]; reply: Reply } {
  const sent: ReadReply[] = []
  return { sent, reply: (message) => void sent.push(message) }
}

describe('answerRead', () => {
  test('answers with the bytes and keeps the same ones', async () => {
    const kept: Kept[] = []
    const { sent, reply } = collector()
    await answerRead(reader(kept), ask(), reply)

    expect(sent).toHaveLength(1)
    const [answer] = sent
    if (answer.type !== 'bytes') throw new Error('expected bytes')
    expect(answer.reqId).toBe(7)
    expect(answer.bytes.byteLength).toBe(1024)

    // Labelled with where the bytes came from, not where the next read starts:
    // the difference between seeding a file and seeding a shifted copy of it.
    expect(kept).toHaveLength(1)
    expect(kept[0].index).toBe(3)
    expect(kept[0].offset).toBe(65536n)
    expect(Array.from(kept[0].bytes.slice(0, 3))).toEqual([65536 % 251, 65537 % 251, 65538 % 251])
  })

  /**
   * The transfer detaches the copy that goes to the worker, never the buffer
   * handed to `keep` — otherwise seeding would store an empty chunk and the
   * verification in `store_row_and_chunks` would silently drop it.
   */
  test('what is kept survives the reply being transferred', async () => {
    const kept: Kept[] = []
    const sent: ReadReply[] = []
    await answerRead(reader(kept), ask(), (message, transfer = []) => {
      // What a real `postMessage` does to a transferred buffer.
      for (const item of transfer) void item
      sent.push(message)
    })
    expect(kept[0].bytes.length).toBe(1024)
  })

  test('a read that fails answers with an error and keeps nothing', async () => {
    const kept: Kept[] = []
    const { sent, reply } = collector()
    const failing = {
      read: () => Promise.reject(new Error('the peer went away')),
      keep: (index: number, offset: bigint, bytes: Uint8Array) => {
        kept.push({ index, offset, bytes })
        return Promise.resolve()
      },
    }
    await answerRead(failing, ask(), reply)

    expect(sent).toHaveLength(1)
    expect(sent[0].type).toBe('error')
    if (sent[0].type !== 'error') throw new Error('expected error')
    expect(sent[0].message).toMatch(/went away/)
    expect(kept).toHaveLength(0)
  })

  test('a keep that rejects still answers with the bytes', async () => {
    // Storage can be refused outright — private mode, a full quota — and that
    // must cost seeding rather than the playback the user asked for.
    const kept: Kept[] = []
    const { sent, reply } = collector()
    await answerRead(reader(kept, () => Promise.reject(new Error('quota exceeded'))), ask(), reply)
    expect(sent[0].type).toBe('bytes')
  })

  /**
   * `keep` crosses into wasm, which allocates before it copies — so it can
   * throw before there is a promise to attach `.catch` to.
   */
  test('a keep that throws synchronously still answers with the bytes', async () => {
    const { sent, reply } = collector()
    const throwing = {
      read: (_index: number, _offset: bigint, len: number) =>
        Promise.resolve(new Uint8Array(len)),
      keep: () => {
        throw new Error('out of memory')
      },
    }
    await answerRead(throwing, ask(), reply)
    expect(sent).toHaveLength(1)
    expect(sent[0].type).toBe('bytes')
  })

  test('a keep that never settles does not hold up the reply', async () => {
    const kept: Kept[] = []
    const { sent, reply } = collector()
    await answerRead(reader(kept, () => new Promise<void>(() => undefined)), ask(), reply)
    expect(sent[0].type).toBe('bytes')
  })

  test('a reader with no keep still answers', async () => {
    const { sent, reply } = collector()
    const plain = {
      read: (_index: number, _offset: bigint, len: number) =>
        Promise.resolve(new Uint8Array(len)),
    }
    await answerRead(plain, ask(), reply)
    expect(sent[0].type).toBe('bytes')
  })
})

/**
 * The two halves of the fix, joined.
 *
 * `range.ts` decides *what* to read and this module decides *what to keep* —
 * separately correct and jointly useless if they disagree, because the client
 * stores only chunks it received whole. This drives the worker's real range
 * handling through the real answer path and asks the only question that
 * matters: after playing the file, can this tab serve it?
 */
describe('a preview streamed end to end', () => {
  const CHUNK_BYTES = 64 * 1024

  test('leaves every chunk of the file in the store', async () => {
    const size = 4 * 1024 * 1024
    const entry: StreamEntry = { index: 2, size, mime: 'video/mp4', sourceIsOrigin: true }
    const kept: Kept[] = []
    const client = {
      read: (_index: number, offset: bigint, len: number) =>
        Promise.resolve(new Uint8Array(Math.min(len, size - Number(offset)))),
      keep(index: number, offset: bigint, bytes: Uint8Array) {
        kept.push({ index, offset, bytes })
        return Promise.resolve()
      },
    }

    // What a media element does: a small opening probe for the container
    // header, a scrub to a byte that lines up with nothing, and a run back over
    // the head it skipped. Deliberately *not* one `bytes=0-` for the whole
    // file — that window starts aligned, so it would hide the very thing this
    // is here to catch.
    let reqId = 0
    for (const header of ['bytes=0-1023', 'bytes=100000-', 'bytes=0-199999']) {
      const response = respond(
        new Request('https://example.test/service-worker/id/clip.mp4', { headers: { Range: header } }),
        entry,
        async (offset, len) => {
          const { sent, reply } = collector()
          reqId += 1
          await answerRead(client, { type: 'read', reqId, index: entry.index, offset, len }, reply)
          const answer = sent[0]
          if (answer.type !== 'bytes') throw new Error(answer.message)
          return new Uint8Array(answer.bytes)
        },
      )
      await response.arrayBuffer()
    }

    const stored = new Set<number>()
    for (const { offset, bytes } of kept) {
      const start = Number(offset)
      const end = start + bytes.length
      for (let index = Math.floor(start / CHUNK_BYTES); index * CHUNK_BYTES < end; index += 1) {
        const from = index * CHUNK_BYTES
        if (from >= start && Math.min(from + CHUNK_BYTES, size) <= end) stored.add(index)
      }
    }
    expect(stored.size).toBe(size / CHUNK_BYTES)
  })
})
