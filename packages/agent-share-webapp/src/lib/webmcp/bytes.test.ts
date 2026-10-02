import { describe, expect, test } from 'bun:test'

import {
  clampWindow,
  DEFAULT_READ_LEN,
  describeWindow,
  looksBinary,
  MAX_READ_LEN,
  toBase64,
} from './bytes.ts'

const utf8 = (text: string) => new TextEncoder().encode(text)

describe('deciding what is text', () => {
  test('ordinary text is text', () => {
    expect(looksBinary(utf8('fn main() {}\n'))).toBe(false)
  })

  test('a NUL byte means binary', () => {
    expect(looksBinary(new Uint8Array([0x7f, 0x45, 0x4c, 0x46, 0x00]))).toBe(true)
  })

  /**
   * The reason the test is a NUL scan rather than a strict decode. A window can
   * start or end in the middle of a multi-byte character, and a strict decode
   * would call that binary — so the same text file would read as text at one
   * offset and as base64 at another.
   */
  test('a window that splits a multi-byte character is still text', () => {
    const whole = utf8('héllo wörld — em dash')
    for (let cut = 1; cut < whole.length; cut += 1) {
      expect(looksBinary(whole.subarray(0, cut))).toBe(false)
    }
  })

  test('an empty window is text', () => {
    expect(looksBinary(new Uint8Array(0))).toBe(false)
  })
})

describe('clamping a requested window', () => {
  test('a window inside the file is left alone', () => {
    expect(clampWindow(1000, 100, 200)).toEqual({ offset: 100, len: 200 })
  })

  test('a window running past the end stops at the end', () => {
    expect(clampWindow(1000, 900, 500)).toEqual({ offset: 900, len: 100 })
  })

  // Not an error: a caller walking `nextOffset` to completion lands here.
  test('an offset past the end yields an empty window, not a failure', () => {
    expect(clampWindow(1000, 4000, 100)).toEqual({ offset: 1000, len: 0 })
  })

  test('a request longer than the wire allows is cut to the wire limit', () => {
    expect(clampWindow(10_000_000, 0, 9_999_999).len).toBe(MAX_READ_LEN)
  })

  test('an empty file has nothing to read at offset zero', () => {
    expect(clampWindow(0, 0, DEFAULT_READ_LEN)).toEqual({ offset: 0, len: 0 })
  })
})

describe('describing a window', () => {
  test('text comes back decoded, with no cursor at end of file', () => {
    const bytes = utf8('hello')
    const window = describeWindow(bytes, 0, 5)

    expect(window).toMatchObject({ encoding: 'utf8', text: 'hello', offset: 0, length: 5, eof: true })
    expect(window.nextOffset).toBeUndefined()
    expect(window.data).toBeUndefined()
  })

  test('a partial window carries the cursor to continue from', () => {
    const window = describeWindow(utf8('hello'), 0, 20)

    expect(window.eof).toBe(false)
    expect(window.nextOffset).toBe(5)
  })

  test('the cursor is relative to the file, not to the window', () => {
    const window = describeWindow(utf8('world'), 100, 200)

    expect(window.offset).toBe(100)
    expect(window.nextOffset).toBe(105)
  })

  test('binary comes back as base64 and never as text', () => {
    const window = describeWindow(new Uint8Array([0, 1, 2, 250]), 0, 4)

    expect(window.encoding).toBe('base64')
    expect(window.text).toBeUndefined()
    expect(window.data).toBe(toBase64(new Uint8Array([0, 1, 2, 250])))
  })

  test('the last window of a file reports eof even when it is empty', () => {
    expect(describeWindow(new Uint8Array(0), 500, 500)).toMatchObject({ eof: true, length: 0 })
  })

  /** Walking a file by its own cursor must land exactly on its size. */
  test('following nextOffset covers the file once, with no gaps or overlap', () => {
    const size = 5000
    const source = new Uint8Array(size).map((_, index) => (index % 251) + 1)
    const seen: number[] = []

    let offset: number | undefined = 0
    let guard = 0
    while (offset !== undefined && guard < 100) {
      guard += 1
      const { offset: start, len } = clampWindow(size, offset, 1024)
      const window = describeWindow(source.subarray(start, start + len), start, size)
      seen.push(window.length)
      offset = window.nextOffset
    }

    expect(seen.reduce((total, length) => total + length, 0)).toBe(size)
  })
})

describe('base64', () => {
  test('round-trips arbitrary bytes', () => {
    const bytes = new Uint8Array(1024).map((_, index) => index % 256)
    const decoded = Uint8Array.from(atob(toBase64(bytes)), (char) => char.charCodeAt(0))

    expect(decoded).toEqual(bytes)
  })

  // Encoded by spreading into String.fromCharCode, which has an argument limit.
  test('survives a payload larger than one chunk', () => {
    const bytes = new Uint8Array(200_000).map((_, index) => index % 256)

    expect(atob(toBase64(bytes)).length).toBe(200_000)
  })
})
