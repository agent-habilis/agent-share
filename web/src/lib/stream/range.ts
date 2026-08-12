/**
 * HTTP byte ranges, as a pure function.
 *
 * This is the half of the streaming service worker that carries the bugs, and
 * the only half that can be tested honestly: happy-dom has no service worker, so
 * nothing that touches `ServiceWorkerGlobalScope` can be exercised in the suite.
 * Everything here takes a `Request` and a reader and returns a `Response` —
 * no worker, no globals, no registration.
 *
 * The whole point of answering `Range` at all: a `<video src>` pointed at a Blob
 * URL cannot seek, because the bytes have to exist before the URL does. Pointed
 * at an origin that answers 206, the element does its own seeking for free — and
 * starts playing before the file is complete.
 */

/** The protocol's per-request ceiling, matching `MAX_READ_LEN`. */
const CHUNK = 256 * 1024

/**
 * The chunk store's addressing unit, mirroring `fofoca_chunks::CHUNK_BYTES`.
 *
 * A range module has no business knowing a storage constant, except that it
 * decides the offsets somebody else has to store. The page keeps whatever this
 * worker asks for, and only a chunk received *whole* has an address — so reads
 * that ignore this boundary hand back bytes that cannot be kept, and a file
 * streamed end to end still never becomes seedable. See [`body`].
 *
 * Drift is bounded rather than dangerous: a wrong value here costs seeding and
 * nothing else, because every chunk is checked against the row's hash before it
 * is stored.
 */
const CHUNK_BYTES = 64 * 1024

/** One file this worker can answer for, as the page registered it. */
export interface StreamEntry {
  /** Manifest index — what a `READ` addresses. */
  readonly index: number
  /** Total size, from the manifest. The `Content-Range` denominator. */
  readonly size: number
  /** Content type, so the element knows what it is decoding. */
  readonly mime: string
  /**
   * Whether the bytes come from the ticket's origin or from a seeder.
   *
   * Load-bearing, not informational — see the short-read guard in [`body`].
   */
  readonly sourceIsOrigin: boolean
}

/** Fetch `len` bytes at `offset` of this entry's file. */
export type ReadAt = (offset: number, len: number) => Promise<Uint8Array>

/** Called when the consumer walks away, so the reader can stop. */
export type OnCancel = () => void

/** A half-open byte window, resolved against a known size. */
interface Window {
  readonly start: number
  /** Inclusive, the way `Content-Range` states it. */
  readonly end: number
}

/**
 * Parse one `Range` header against `size`.
 *
 * `null` means "no range asked for", answered with a 200. `'unsatisfiable'`
 * means the request named something outside the file, answered with a 416 —
 * distinct from `null`, because answering a whole body to a client that asked
 * for bytes past the end would look like success and decode as garbage.
 *
 * **Multiple ranges are deliberately ignored** and read as no range at all. No
 * media element asks for them, and a multipart response half-implemented is
 * worse than a complete body.
 */
export function parseRange(
  header: string | null,
  size: number,
): Window | null | 'unsatisfiable' {
  if (!header) return null
  const match = /^bytes=(\d*)-(\d*)$/.exec(header.trim())
  if (!match) return null
  const [, rawStart, rawEnd] = match
  if (rawStart === '' && rawEnd === '') return null

  // A suffix range: the *last* n bytes, which is how a player probes a
  // container's trailing index. `bytes=-0` asks for nothing and cannot be met.
  if (rawStart === '') {
    const wanted = Number(rawEnd)
    if (wanted === 0) return 'unsatisfiable'
    const start = Math.max(0, size - wanted)
    return size === 0 ? 'unsatisfiable' : { start, end: size - 1 }
  }

  const start = Number(rawStart)
  // Past the end is unsatisfiable rather than empty: the client is working from
  // a size that does not match ours, and saying so is the only useful answer.
  if (start >= size) return 'unsatisfiable'
  // Clamped rather than refused. An open-ended `bytes=N-` is the common case,
  // and a player that overshoots the end is asking for "the rest".
  const end = rawEnd === '' ? size - 1 : Math.min(Number(rawEnd), size - 1)
  if (end < start) return 'unsatisfiable'
  return { start, end }
}

/**
 * The bytes of `window`, walked in protocol-sized steps.
 *
 * `cancel` is not hygiene. A media element drops and reopens its range request
 * on **every** seek, so a stream that ignored cancellation would leave one live
 * read loop per scrub running against the peer for the life of the page. The
 * read already in flight cannot be aborted; what this does is stop issuing more,
 * which bounds an abandoned seek at one outstanding chunk.
 *
 * # Why the steps end on chunk boundaries
 *
 * A window starts wherever the element seeked to, which is almost never a
 * multiple of [`CHUNK_BYTES`]. Walked in flat [`CHUNK`] steps, every read spans
 * four chunks and holds three of them whole; the fourth is split across this
 * read and the next, so the page — which keeps only whole chunks — drops it
 * twice. Every fourth chunk is then never stored, coverage stops at 75 %, and
 * the file never becomes servable no matter how long it plays.
 *
 * Ending each full step on a boundary costs one short read at the head of a
 * window and nothing after it. Only the chunk the window's own start cuts
 * through is lost, and a later `sync` refetches that one for the price of a
 * chunk.
 */
/**
 * How many bytes to ask for at `offset`, capped and boundary-aligned.
 *
 * Asked per read from the live offset rather than worked out as a schedule up
 * front, because a read may answer with fewer bytes than it was given — and the
 * next one has to align from where the bytes actually stopped.
 *
 * The last read of a window is returned unaligned on purpose: its tail is the
 * end of what was asked for, so trimming it to a boundary would only buy one
 * more round trip.
 */
function step(offset: number, window: Window): number {
  const remaining = window.end - offset + 1
  if (remaining <= CHUNK) return remaining
  const aligned = Math.floor((offset + CHUNK) / CHUNK_BYTES) * CHUNK_BYTES
  // A full step always clears the next boundary, so this holds — it is here so
  // that a smaller `CHUNK` can never make the walk stall on a zero-length read.
  return aligned > offset ? aligned - offset : CHUNK
}

function body(entry: StreamEntry, window: Window, read: ReadAt, onCancel?: OnCancel) {
  let offset = window.start
  let cancelled = false
  return new ReadableStream<Uint8Array>({
    async pull(controller) {
      if (cancelled) return
      if (offset > window.end) {
        controller.close()
        return
      }
      const chunk = await read(offset, step(offset, window))
      if (cancelled) return
      if (chunk.length === 0) {
        if (!entry.sourceIsOrigin) {
          // Carried over verbatim from the download path. A seeder serves a
          // frozen snapshot, so stopping short of the size that snapshot
          // describes is truncation, not EOF — and closing here would hand a
          // silently truncated file to a decoder, which is the failure mode
          // this whole guard exists to prevent.
          controller.error(
            new Error('the seeder stopped short of the size the manifest describes'),
          )
          return
        }
        controller.close()
        return
      }
      offset += chunk.length
      controller.enqueue(chunk)
    },
    cancel() {
      cancelled = true
      onCancel?.()
    },
  })
}

/** Headers every answer carries, range or not. */
function base(entry: StreamEntry): Record<string, string> {
  return {
    'Content-Type': entry.mime || 'application/octet-stream',
    // Advertised on every response, including the 200: it is how an element
    // learns it may seek at all, and a player that never sees it will not try.
    'Accept-Ranges': 'bytes',
    // These bytes came off a peer connection this page authenticated; nothing
    // downstream should keep a copy addressed by a URL that outlives the tab.
    'Cache-Control': 'no-store',
  }
}

/**
 * Answer one request for `entry`.
 *
 * Pure with respect to the worker: everything it needs arrives as an argument,
 * which is what lets the suite exercise every row of the range table without a
 * `ServiceWorkerGlobalScope`.
 */
export function respond(
  request: Request,
  entry: StreamEntry,
  read: ReadAt,
  onCancel?: OnCancel,
): Response {
  const window = parseRange(request.headers.get('Range'), entry.size)

  if (window === 'unsatisfiable') {
    return new Response(null, {
      status: 416,
      headers: { ...base(entry), 'Content-Range': `bytes */${entry.size}` },
    })
  }

  if (window === null) {
    // A zero-byte file lands here too, and correctly: there is no satisfiable
    // range in an empty file, so the whole-body answer is the only one.
    return new Response(
      entry.size === 0 ? null : body(entry, { start: 0, end: entry.size - 1 }, read, onCancel),
      {
        status: 200,
        headers: { ...base(entry), 'Content-Length': String(entry.size) },
      },
    )
  }

  const length = window.end - window.start + 1
  return new Response(body(entry, window, read, onCancel), {
    status: 206,
    headers: {
      ...base(entry),
      'Content-Length': String(length),
      // Inclusive at both ends. The off-by-one that breaks seeking lives in
      // this line and in `length` above.
      'Content-Range': `bytes ${window.start}-${window.end}/${entry.size}`,
    },
  })
}
