import { describe, expect, test } from 'bun:test'

import { parseRange, respond, type StreamEntry } from './range.ts'

function entry(over: Partial<StreamEntry> = {}): StreamEntry {
  return { index: 3, size: 1000, mime: 'video/mp4', sourceIsOrigin: true, ...over }
}

/** A reader that answers with position-derived bytes, in protocol-sized steps. */
function reader(size: number) {
  const asked: Array<[number, number]> = []
  const read = async (offset: number, len: number) => {
    asked.push([offset, len])
    const take = Math.max(0, Math.min(len, size - offset))
    return new Uint8Array(take).map((_, index) => (offset + index) % 251)
  }
  return { read, asked }
}

/** A reader that stops short once, to exercise the re-alignment path. */
function truncating(size: number, truncateFirstTo: number) {
  const asked: Array<[number, number]> = []
  const read = async (offset: number, len: number) => {
    const want = asked.length === 0 ? Math.min(len, truncateFirstTo) : len
    asked.push([offset, len])
    const take = Math.max(0, Math.min(want, size - offset))
    return new Uint8Array(take).map((_, index) => (offset + index) % 251)
  }
  return { read, asked }
}

function ranged(header: string | null, size = 1000): Request {
  void size
  return new Request('https://example.test/service-worker/abc/clip.mp4', {
    headers: header ? { Range: header } : {},
  })
}

/**
 * `fofoca_chunks::CHUNK_BYTES`, mirrored here for the same reason `range.ts`
 * mirrors it: these tests assert what the page will be able to *keep*.
 */
const CHUNK_BYTES = 64 * 1024

/**
 * Which chunks a log of reads would leave in the store.
 *
 * The predicate is `store_row_and_chunks`'s, verbatim: a chunk is kept only
 * when it lies **wholly** inside a single read, because half a chunk has no
 * address. This is what turns a read schedule into a coverage claim.
 */
function kept(asked: Array<[number, number]>, size: number): Set<number> {
  const chunks = new Set<number>()
  for (const [offset, len] of asked) {
    const end = Math.min(offset + len, size)
    for (let index = Math.floor(offset / CHUNK_BYTES); index * CHUNK_BYTES < end; index += 1) {
      const start = index * CHUNK_BYTES
      if (start >= offset && Math.min(start + CHUNK_BYTES, size) <= end) chunks.add(index)
    }
  }
  return chunks
}

describe('parseRange', () => {
  test('no header means no range', () => {
    expect(parseRange(null, 1000)).toBeNull()
    expect(parseRange('', 1000)).toBeNull()
  })

  test('open-ended runs to the last byte', () => {
    expect(parseRange('bytes=0-', 1000)).toEqual({ start: 0, end: 999 })
    expect(parseRange('bytes=500-', 1000)).toEqual({ start: 500, end: 999 })
  })

  /** The off-by-one that breaks seeking: both ends are inclusive. */
  test('an explicit window is inclusive at both ends', () => {
    expect(parseRange('bytes=100-199', 1000)).toEqual({ start: 100, end: 199 })
    expect(parseRange('bytes=0-0', 1000)).toEqual({ start: 0, end: 0 })
  })

  test('a suffix range is the last n bytes', () => {
    expect(parseRange('bytes=-500', 1000)).toEqual({ start: 500, end: 999 })
    // Longer than the file: the whole of it, not a negative start.
    expect(parseRange('bytes=-5000', 1000)).toEqual({ start: 0, end: 999 })
  })

  test('an end past the file is clamped, not refused', () => {
    expect(parseRange('bytes=900-99999', 1000)).toEqual({ start: 900, end: 999 })
  })

  /**
   * Distinct from "no range": a client asking past the end is working from a
   * size that disagrees with ours, and a whole body would decode as garbage
   * while looking like success.
   */
  test('a start past the file is unsatisfiable', () => {
    expect(parseRange('bytes=1000-', 1000)).toBe('unsatisfiable')
    expect(parseRange('bytes=2000-3000', 1000)).toBe('unsatisfiable')
    expect(parseRange('bytes=-0', 1000)).toBe('unsatisfiable')
    expect(parseRange('bytes=0-', 0)).toBe('unsatisfiable')
  })

  /** No media element asks, and a half-honoured multipart is worse than a body. */
  test('multiple ranges are ignored rather than half-answered', () => {
    expect(parseRange('bytes=0-99,200-299', 1000)).toBeNull()
  })

  test('a malformed header is ignored rather than throwing', () => {
    expect(parseRange('bananas', 1000)).toBeNull()
    expect(parseRange('bytes=abc-def', 1000)).toBeNull()
  })
})

describe('respond', () => {
  test('no range answers 200 with the length and an offer to seek', async () => {
    const { read } = reader(1000)
    const response = respond(ranged(null), entry(), read)
    expect(response.status).toBe(200)
    expect(response.headers.get('Content-Length')).toBe('1000')
    expect(response.headers.get('Accept-Ranges')).toBe('bytes')
    expect(response.headers.get('Content-Type')).toBe('video/mp4')
    expect((await response.arrayBuffer()).byteLength).toBe(1000)
  })

  test('a window answers 206 with an inclusive Content-Range', async () => {
    const { read } = reader(1000)
    const response = respond(ranged('bytes=100-199'), entry(), read)
    expect(response.status).toBe(206)
    expect(response.headers.get('Content-Range')).toBe('bytes 100-199/1000')
    expect(response.headers.get('Content-Length')).toBe('100')
    expect((await response.arrayBuffer()).byteLength).toBe(100)
  })

  test('the body starts at the requested offset, not at zero', async () => {
    const { read } = reader(1000)
    const response = respond(ranged('bytes=100-109'), entry(), read)
    const bytes = new Uint8Array(await response.arrayBuffer())
    expect(Array.from(bytes.slice(0, 3))).toEqual([100, 101, 102])
  })

  test('unsatisfiable answers 416 naming the real size', async () => {
    const { read } = reader(1000)
    const response = respond(ranged('bytes=5000-'), entry(), read)
    expect(response.status).toBe(416)
    expect(response.headers.get('Content-Range')).toBe('bytes */1000')
  })

  test('an empty file answers 200 with nothing in it', async () => {
    const { read } = reader(0)
    const response = respond(ranged(null), entry({ size: 0 }), read)
    expect(response.status).toBe(200)
    expect(response.headers.get('Content-Length')).toBe('0')
    expect((await response.arrayBuffer()).byteLength).toBe(0)
  })

  test('reads are issued in protocol-sized steps', async () => {
    const { read, asked } = reader(700 * 1024)
    const response = respond(ranged(null), entry({ size: 700 * 1024 }), read)
    await response.arrayBuffer()
    expect(asked.length).toBe(3)
    expect(asked[0]).toEqual([0, 256 * 1024])
    // The tail is short, never a full chunk past the end.
    expect(asked[2][1]).toBe(700 * 1024 - 2 * 256 * 1024)
  })

  /**
   * The one that pins the bug this alignment exists for.
   *
   * A media element seeks to an arbitrary byte, so the window it asks for is
   * almost never chunk-aligned. Walked in flat 256 KiB steps, every fourth
   * chunk straddles two reads and is dropped by both — coverage plateaus at
   * 75 %, the file never completes, and the tab can never advertise it whole
   * however long it plays.
   */
  test('an unaligned window still keeps every chunk it passed over', async () => {
    const size = 4 * 1024 * 1024
    const { read, asked } = reader(size)
    const response = respond(ranged('bytes=100000-'), entry({ size }), read)
    await response.arrayBuffer()

    const chunks = kept(asked, size)
    // Chunk 1 is cut through by the window's start, so it is genuinely
    // unreachable here. Everything from chunk 2 on was fully transferred and
    // must therefore be stored.
    const reachable = Array.from({ length: size / CHUNK_BYTES - 2 }, (_, i) => i + 2)
    expect(reachable.filter((index) => !chunks.has(index))).toEqual([])
  })

  test('every read but the first starts and ends on a chunk boundary', async () => {
    const size = 4 * 1024 * 1024
    const { read, asked } = reader(size)
    await respond(ranged('bytes=100000-'), entry({ size }), read).arrayBuffer()

    expect(asked[0][0]).toBe(100000)
    for (const [offset] of asked.slice(1)) expect(offset % CHUNK_BYTES).toBe(0)
    // The last read stops at the file's end, which is a boundary only by luck.
    for (const [offset, len] of asked.slice(0, -1)) {
      expect((offset + len) % CHUNK_BYTES).toBe(0)
    }
    const [offset, len] = asked[asked.length - 1]
    expect(offset + len).toBe(size)
  })

  /** Alignment must not cost a round trip: the tail is already whole. */
  test('aligning adds no extra read at the end of a window', async () => {
    const size = 4 * 1024 * 1024
    const { read, asked } = reader(size)
    await respond(ranged('bytes=100000-'), entry({ size }), read).arrayBuffer()
    expect(asked.length).toBe(Math.ceil((size - 100000) / (256 * 1024)))
  })

  /** Shaping the reads must not change a single byte of the answer. */
  test('an unaligned window answers exactly what it was asked for', async () => {
    const size = 4 * 1024 * 1024
    const { read } = reader(size)
    const response = respond(ranged('bytes=100000-200000'), entry({ size }), read)
    expect(response.status).toBe(206)
    expect(response.headers.get('Content-Range')).toBe(`bytes 100000-200000/${size}`)
    expect(response.headers.get('Content-Length')).toBe('100001')
    const bytes = new Uint8Array(await response.arrayBuffer())
    expect(bytes.length).toBe(100001)
    expect(Array.from(bytes.slice(0, 3))).toEqual([100000 % 251, 100001 % 251, 100002 % 251])
    expect(bytes[bytes.length - 1]).toBe(200000 % 251)
  })

  /** `want` must stay positive: a window smaller than a chunk has no boundary. */
  test('a window shorter than one chunk is read in one go', async () => {
    const size = 4 * 1024 * 1024
    const { read, asked } = reader(size)
    await respond(ranged('bytes=100000-100000'), entry({ size }), read).arrayBuffer()
    expect(asked).toEqual([[100000, 1]])
  })

  /**
   * The reason alignment is computed per pull rather than as a schedule up
   * front: a read may legitimately answer with fewer bytes than it was asked
   * for, and the next one has to align from where the bytes actually stopped.
   */
  test('a short read re-aligns the next one rather than staying off by it', async () => {
    const size = 4 * 1024 * 1024
    const { read, asked } = truncating(size, 1000)
    await respond(ranged('bytes=100000-'), entry({ size }), read).arrayBuffer()
    expect(asked[1][0]).toBe(101000)
    expect((asked[1][0] + asked[1][1]) % CHUNK_BYTES).toBe(0)
  })

  /**
   * The guard carried over from the download path. A seeder serves a frozen
   * snapshot, so a short answer is truncation — and a decoder handed a
   * truncated file fails in a way that names the wrong problem.
   */
  test('a seeder that stops short errors rather than closing', async () => {
    const short = async () => new Uint8Array(0)
    const response = respond(ranged(null), entry({ sourceIsOrigin: false }), short)
    await expect(response.arrayBuffer()).rejects.toThrow(/stopped short/)
  })

  test('the origin stopping short is EOF, not an error', async () => {
    const short = async () => new Uint8Array(0)
    const response = respond(ranged(null), entry({ sourceIsOrigin: true }), short)
    expect((await response.arrayBuffer()).byteLength).toBe(0)
  })

  /**
   * Load-bearing: a media element reopens its range request on every seek, so
   * without this each scrub would leave a read loop running against the peer.
   */
  test('cancelling the body stops the reader and says so', async () => {
    const { read, asked } = reader(10 * 1024 * 1024)
    let cancelled = false
    const response = respond(ranged(null), entry({ size: 10 * 1024 * 1024 }), read, () => {
      cancelled = true
    })
    const stream = response.body
    if (!stream) throw new Error('expected a body')
    const cursor = stream.getReader()
    await cursor.read()
    const issued = asked.length
    await cursor.cancel()
    expect(cancelled).toBe(true)
    expect(asked.length).toBe(issued)
  })
})
