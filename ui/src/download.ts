/**
 * Download a whole folder as a ZIP, streamed.
 *
 * Never buffered: a share can be far larger than memory, and the protocol caps
 * a single read at 256 KiB anyway, so files arrive in chunks and go straight
 * out. Where the File System Access API exists the ZIP is written directly to
 * disk; otherwise it falls back to a Blob, which *is* memory-bound — so the
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
 */
function fileStream(reader: Reader, file: FileNode): ReadableStream<Uint8Array> {
  let offset = 0
  return new ReadableStream({
    async pull(controller) {
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
 */
export function zipStream(
  reader: Reader,
  files: FileNode[],
  onProgress?: (progress: Progress) => void,
): ReadableStream<Uint8Array> {
  const total = files.reduce((sum, file) => sum + file.size, 0)
  let done = 0

  const counted: Reader = {
    async read(index, offset, len) {
      const chunk = await reader.read(index, offset, len)
      done += chunk.length
      onProgress?.({ done, total })
      return chunk
    },
  }

  return downloadZip(
    files.map((file) => ({
      name: file.path,
      // A zero mtime means "unknown" on the wire; passing it through would
      // date every such file to 1970.
      lastModified: file.mtime > 0 ? new Date(file.mtime * 1000) : new Date(),
      input: fileStream(counted, file),
    })),
  ).body as ReadableStream<Uint8Array>
}

/**
 * Save the ZIP, preferring a direct-to-disk stream.
 *
 * @returns `true` when it streamed to disk, `false` when it fell back to a
 * Blob (and therefore held the whole archive in memory).
 */
export async function saveZip(
  stream: ReadableStream<Uint8Array>,
  suggestedName: string,
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
    await stream.pipeTo(writable)
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
