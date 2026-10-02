/**
 * Turning a window of file bytes into something a model can read.
 *
 * Every read is bounded and carries a cursor. Two limits meet here: the wire
 * refuses a READ longer than 256 KiB (`MAX_READ_LEN` in the protocol), and a
 * tool result is model context, where a whole file is rarely what was wanted.
 * So the default window is much smaller than the maximum, and the caller walks
 * a file by following `nextOffset` rather than by asking for all of it.
 */

/** The protocol's cap on one READ. Asking for more is a wire error, not a slow read. */
export const MAX_READ_LEN = 262144

/** What a read returns when the caller does not say. 64 KiB. */
export const DEFAULT_READ_LEN = 65536

export interface ReadWindow {
  /** `utf8` when the bytes decode as text, `base64` when they do not. */
  encoding: 'utf8' | 'base64'
  /** Present for `utf8`. */
  text?: string
  /** Present for `base64`. */
  data?: string
  offset: number
  /** Bytes in this window, not in the file. */
  length: number
  size: number
  eof: boolean
  /** Where to continue. Absent at end of file. */
  nextOffset?: number
}

/**
 * Whether these bytes should be handed over as text.
 *
 * A NUL byte is the test, which is what `grep` uses and for the same reason: it
 * is the one signal that survives being applied to an arbitrary *window* of a
 * file rather than the whole thing. Decoding strictly and catching the failure
 * would misread every text file whose window happens to split a multi-byte
 * character — a false "this is binary" that changes with the offset asked for.
 */
export function looksBinary(bytes: Uint8Array): boolean {
  return bytes.includes(0)
}

const BASE64_CHUNK = 0x8000

export function toBase64(bytes: Uint8Array): string {
  let binary = ''
  for (let index = 0; index < bytes.length; index += BASE64_CHUNK) {
    binary += String.fromCharCode(...bytes.subarray(index, index + BASE64_CHUNK))
  }
  return btoa(binary)
}

/**
 * Clamp a requested window to the file and to the wire.
 *
 * An `offset` past the end is not an error: it yields a zero-length window at
 * end of file, which is what a caller walking `nextOffset` to completion sees
 * on its last step.
 */
export function clampWindow(
  size: number,
  offset: number,
  len: number,
): { offset: number; len: number } {
  const start = Math.min(offset, size)
  return { offset: start, len: Math.min(len, MAX_READ_LEN, Math.max(size - start, 0)) }
}

export function describeWindow(bytes: Uint8Array, offset: number, size: number): ReadWindow {
  const end = offset + bytes.length
  const eof = end >= size
  const window: ReadWindow = {
    encoding: looksBinary(bytes) ? 'base64' : 'utf8',
    offset,
    length: bytes.length,
    size,
    eof,
  }
  if (window.encoding === 'base64') {
    window.data = toBase64(bytes)
  } else {
    window.text = new TextDecoder().decode(bytes)
  }
  if (!eof) window.nextOffset = end
  return window
}
