/**
 * Download a selection, streamed: a single file as itself, a folder as a ZIP.
 *
 * Never buffered: a share can be far larger than memory, and the protocol caps
 * a single read at 256 KiB anyway, so files arrive in chunks and go straight
 * out. Where the File System Access API exists the bytes are written directly
 * to disk; otherwise it falls back to a Blob, which *is* memory-bound — so the
 * fallback is a real limitation, not a footnote.
 */

import { downloadZip } from 'client-zip'

import type { FileNode } from '../tree.ts'

/** The protocol's per-request ceiling (`MAX_READ_LEN`). */
const CHUNK = 256 * 1024

interface Reader {
  read(index: number, offset: bigint, len: number): Promise<Uint8Array>
  /**
   * Whether the mount reaches the ticket's origin. Absent reads as `true`.
   * From a seeder, a short read is a failure (guard #2), not EOF — see
   * `fileStream`.
   */
  readonly source_is_origin?: boolean
  /**
   * Hand bytes that just arrived back to the client, so this tab can seed them.
   *
   * The whole point of routing every path through `fileStream`: a download, a
   * folder-as-ZIP and a preview all pull the same bytes, and without this they
   * were thrown away — so pressing Seed afterwards pulled them a second time.
   *
   * Optional because a plain reader has nowhere to put them, and because the
   * call must never be load-bearing: see `keepChunks` for why its failures are
   * swallowed.
   */
  keep?(index: number, offset: bigint, bytes: Uint8Array): Promise<void>
}

/**
 * Feed fetched bytes to the client's chunk store, and never let it matter.
 *
 * Fire-and-forget on purpose, in both directions:
 *
 * - **Not awaited**, so storing never sits between two reads and slows a
 *   transfer down. The bytes are already in hand; keeping them is bookkeeping.
 * - **Never rethrown**, so a full quota or a private-mode refusal costs seeding
 *   and not the download. A user who asked for a file gets the file.
 *
 * The client keeps only chunks lying wholly inside what it is given, so the
 * sequential 256 KiB pieces this sends — exactly four aligned 64 KiB chunks —
 * are kept in full.
 */
function keepChunks(reader: Reader, index: number, offset: number, bytes: Uint8Array): void {
  if (!reader.keep) return
  void reader.keep(index, BigInt(offset), bytes).catch((error: unknown) => {
    console.debug('[share] keeping a chunk failed; not seeding these bytes', error)
  })
}

export interface Progress {
  /** Bytes delivered so far. */
  done: number
  /** Bytes the manifest says are coming. */
  total: number
}

/**
 * Stream one file's bytes, chunk by chunk.
 *
 * Stops on a short read: past-EOF is a valid empty read in this protocol, so
 * an empty chunk means the file ended, not that something failed.
 *
 * `signal` is checked per chunk rather than left to `pipeTo` alone: the Blob
 * fallback in `pickSaveTarget` has no pipe to abort, so this is the only place
 * a cancel can reach it.
 */
function fileStream(
  reader: Reader,
  file: FileNode,
  signal?: AbortSignal,
): ReadableStream<Uint8Array> {
  let offset = 0
  return new ReadableStream({
    async pull(controller) {
      if (signal?.aborted) {
        controller.error(signal.reason)
        return
      }
      if (offset >= file.size) {
        controller.close()
        return
      }
      const want = Math.min(CHUNK, file.size - offset)
      const chunk = await reader.read(file.index, BigInt(offset), want)
      if (chunk.length === 0) {
        if (reader.source_is_origin === false) {
          // A seeder serves a frozen snapshot; stopping short of the size
          // that snapshot describes is a failure, and closing here would
          // deliver a silently truncated download.
          controller.error(
            new Error(
              `${file.path}: the seeder stopped short of the size the manifest describes`,
            ),
          )
          return
        }
        controller.close()
        return
      }
      // Kept before the offset moves, so the bytes are labelled with where they
      // actually came from rather than where the next read will start.
      keepChunks(reader, file.index, offset, chunk)
      offset += chunk.length
      controller.enqueue(chunk)
    },
  })
}

/**
 * Build the ZIP stream for `files`.
 *
 * `onProgress` is called as bytes land. The caller owns progress reporting;
 * the wasm client deliberately exposes no callback of its own.
 *
 * The entries are yielded from a generator rather than built with `.map()`,
 * and that is load-bearing rather than style. A `ReadableStream` with the
 * default queuing strategy calls `pull` as soon as it is *constructed* — no
 * reader required, because its desired size is already 1. So an array of
 * entries opened one read per file the instant this function was called, all
 * of them in flight before the zipper had asked for anything. Three files made
 * three; a share with more files than the producer's concurrent-stream ceiling
 * (100, iroh's default) would have stalled on the first tick. Lazily, each
 * file's stream is built only when the zipper reaches it.
 */
export function zipStream(
  reader: Reader,
  files: FileNode[],
  onProgress?: (progress: Progress) => void,
  signal?: AbortSignal,
): ReadableStream<Uint8Array> {
  const total = files.reduce((sum, file) => sum + file.size, 0)
  const counted = countingReader(reader, total, onProgress)

  function* entries() {
    for (const file of files) {
      yield {
        name: file.path,
        // A zero mtime means "unknown" on the wire; passing it through would
        // date every such file to 1970.
        lastModified: file.mtime > 0 ? new Date(file.mtime * 1000) : new Date(),
        input: fileStream(counted, file, signal),
      }
    }
  }

  return downloadZip(entries()).body as ReadableStream<Uint8Array>
}

/**
 * Stream a single file's bytes as-is — no archive around them.
 *
 * A one-file download wrapped in a ZIP is pure friction: the receiver wants
 * the file, not an unpacking step. Same chunked reads and progress as the ZIP
 * path, minus the container.
 */
export function singleFileStream(
  reader: Reader,
  file: FileNode,
  onProgress?: (progress: Progress) => void,
  signal?: AbortSignal,
): ReadableStream<Uint8Array> {
  return fileStream(countingReader(reader, file.size, onProgress), file, signal)
}

/** Wrap `reader` so every chunk advances a shared progress counter. */
function countingReader(
  reader: Reader,
  total: number,
  onProgress?: (progress: Progress) => void,
): Reader {
  let done = 0
  return {
    async read(index, offset, len) {
      const chunk = await reader.read(index, offset, len)
      done += chunk.length
      onProgress?.({ done, total })
      return chunk
    },
    // Forwarded, not defaulted: the wrapper must not launder a seeder into
    // looking like the origin, or guard #2 silently switches off.
    source_is_origin: reader.source_is_origin,
    // Forwarded for the same reason in the other direction: a wrapper that
    // dropped this would silently turn seeding off for every path that goes
    // through it, which is all of them.
    keep: reader.keep ? (index, offset, bytes) => reader.keep!(index, offset, bytes) : undefined,
  }
}

/** Somewhere to put the bytes, chosen before any of them are asked for. */
export interface SaveTarget {
  /**
   * `true` when bytes go straight to disk, `false` when they go through a Blob
   * and are therefore held whole in memory.
   */
  readonly toDisk: boolean
  /**
   * Consume `stream` into the chosen destination.
   *
   * Aborting `signal` tears down the pipe, which aborts the writable rather
   * than closing it — so a cancelled download never commits a partial file.
   */
  write(stream: ReadableStream<Uint8Array>, signal?: AbortSignal): Promise<void>
}

/**
 * Ask where to save. AbortError propagates for a dismissed dialog.
 *
 * Picking is separate from writing so the caller can put the dialog *before*
 * the stream exists. Both halves matter: a `ReadableStream` pulls as soon as it
 * is constructed (see `zipStream`), so building one first means a dismissed
 * dialog has already opened reads against the peer — and the caller cannot show
 * a progress bar for a transfer that has nowhere to go yet.
 */
export async function pickSaveTarget(suggestedName: string): Promise<SaveTarget> {
  const picker = window.showSaveFilePicker
  if (picker) {
    const handle = await picker({ suggestedName })
    return {
      toDisk: true,
      async write(stream, signal) {
        const writable = await handle.createWritable()
        await stream.pipeTo(writable, signal ? { signal } : undefined)
      },
    }
  }

  // No File System Access API (Safari, Firefox): nothing to pick, so the whole
  // download lands in memory and leaves through an anchor.
  return {
    toDisk: false,
    async write(stream) {
      const blob = await new Response(stream).blob()
      const url = URL.createObjectURL(blob)
      const anchor = document.createElement('a')
      anchor.href = url
      anchor.download = suggestedName
      anchor.click()
      URL.revokeObjectURL(url)
    },
  }
}
