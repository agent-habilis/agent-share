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

import type { FileNode } from './tree.ts'

/** The protocol's per-request ceiling (`MAX_READ_LEN`). */
const CHUNK = 256 * 1024

interface Reader {
  read(index: number, offset: bigint, len: number): Promise<Uint8Array>
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
 * fallback in `saveStream` has no pipe to abort, so this is the only place a
 * cancel can reach it.
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
        controller.close()
        return
      }
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
  }
}

/**
 * Save the stream, preferring a direct-to-disk pipe.
 *
 * Aborting `signal` tears down the pipe, which aborts the writable rather than
 * closing it — so a cancelled download never commits a partial file.
 *
 * @returns `true` when it streamed to disk, `false` when it fell back to a
 * Blob (and therefore held the whole download in memory).
 */
export async function saveStream(
  stream: ReadableStream<Uint8Array>,
  suggestedName: string,
  signal?: AbortSignal,
): Promise<boolean> {
  const picker = (
    window as unknown as {
      showSaveFilePicker?: (options: { suggestedName: string }) => Promise<{
        createWritable(): Promise<WritableStream<Uint8Array>>
      }>
    }
  ).showSaveFilePicker

  if (picker) {
    const handle = await picker({ suggestedName })
    const writable = await handle.createWritable()
    await stream.pipeTo(writable, signal ? { signal } : undefined)
    return true
  }

  const blob = await new Response(stream).blob()
  const url = URL.createObjectURL(blob)
  const anchor = document.createElement('a')
  anchor.href = url
  anchor.download = suggestedName
  anchor.click()
  URL.revokeObjectURL(url)
  return false
}
