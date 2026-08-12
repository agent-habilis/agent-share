import { describe, expect, test } from 'bun:test'

import {
  fail,
  guard,
  ok,
  optionalBool,
  optionalInt,
  optionalString,
  requiredString,
  sharePathParts,
  ToolInputError,
} from './result.ts'

/**
 * The browser validates neither side of a tool call: it does not check input
 * against `inputSchema`, and it replaces whatever a tool throws with a generic
 * "the invocation failed". These cases pin the two habits that follow from
 * that — check the arguments, and return the failure instead of throwing.
 */

describe('guard turns a throw into an answer', () => {
  test('an unexpected throw becomes a failure result, not a rejection', async () => {
    const result = await guard(async () => {
      throw new Error('the connection went away')
    })

    expect(result.ok).toBe(false)
    expect(result).toMatchObject({ code: 'failed', error: 'the connection went away' })
  })

  test('a ToolInputError keeps the code it chose', async () => {
    const result = await guard(async () => {
      throw new ToolInputError('not_found', '"nope.txt" is not in this share')
    })

    expect(result).toMatchObject({ ok: false, code: 'not_found' })
  })

  test('a success passes through untouched', async () => {
    expect(await guard(async () => ok({ files: 3 }))).toEqual({ ok: true, files: 3 })
  })

  test('a thrown non-Error still reads as something', async () => {
    const result = await guard(async () => {
      throw 'just a string'
    })

    expect(result).toMatchObject({ ok: false, error: 'just a string' })
  })

  // A model reading a 40 KB stack trace learns nothing and pays for all of it.
  test('an enormous throw is cut down before it reaches the agent', async () => {
    const result = await guard(async () => {
      throw 'x'.repeat(5000)
    })

    expect(result.ok).toBe(false)
    if (result.ok) return
    expect(result.error.length).toBeLessThan(400)
    expect(result.error.endsWith('…')).toBe(true)
  })
})

describe('argument checking', () => {
  test('a missing required string is named in the complaint', () => {
    expect(() => requiredString({}, 'path')).toThrow(/"path" is required/)
  })

  test('a blank or whitespace-only string does not count as given', () => {
    expect(() => requiredString({ path: '   ' }, 'path')).toThrow(/"path" is required/)
  })

  test('a required string is trimmed', () => {
    expect(requiredString({ path: '  src/lib.rs  ' }, 'path')).toBe('src/lib.rs')
  })

  test('an absent optional string is undefined, not empty', () => {
    expect(optionalString({}, 'ticket')).toBeUndefined()
    expect(optionalString({ ticket: '  ' }, 'ticket')).toBeUndefined()
  })

  test('integers are range-checked and must be whole', () => {
    expect(optionalInt({}, 'depth', { min: 1, max: 10, fallback: 1 })).toBe(1)
    expect(optionalInt({ depth: 4 }, 'depth', { min: 1, max: 10, fallback: 1 })).toBe(4)
    expect(() => optionalInt({ depth: 99 }, 'depth', { min: 1, max: 10, fallback: 1 })).toThrow(
      /between 1 and 10/,
    )
    expect(() => optionalInt({ depth: 1.5 }, 'depth', { min: 1, max: 10, fallback: 1 })).toThrow(
      /whole number/,
    )
  })

  // An agent that JSON-encodes its arguments can send "2" where 2 was meant.
  test('a numeric string is accepted as the number it spells', () => {
    expect(optionalInt({ depth: '3' }, 'depth', { min: 1, max: 10, fallback: 1 })).toBe(3)
    expect(() => optionalInt({ depth: 'deep' }, 'depth', { min: 1, max: 10, fallback: 1 })).toThrow(
      /must be a number/,
    )
  })

  test('booleans take their string spellings too', () => {
    expect(optionalBool({}, 'refresh', false)).toBe(false)
    expect(optionalBool({ refresh: true }, 'refresh', false)).toBe(true)
    expect(optionalBool({ refresh: 'true' }, 'refresh', false)).toBe(true)
    expect(optionalBool({ refresh: 'false' }, 'refresh', true)).toBe(false)
    expect(() => optionalBool({ refresh: 'maybe' }, 'refresh', false)).toThrow(/must be true or false/)
  })
})

describe('share paths', () => {
  test('an empty path means the share root', () => {
    expect(sharePathParts(undefined)).toEqual([])
    expect(sharePathParts('')).toEqual([])
    expect(sharePathParts('/')).toEqual([])
    expect(sharePathParts('.')).toEqual([])
  })

  test('leading and trailing slashes are not part of the path', () => {
    expect(sharePathParts('/src/lib.rs')).toEqual(['src', 'lib.rs'])
    expect(sharePathParts('src/nested/')).toEqual(['src', 'nested'])
  })

  // The manifest is a remote peer's word for what it holds, so a path that
  // climbs out of the share is rejected before anything looks it up.
  test.each([
    ['..', '..'],
    ['climbing out', '../../etc/passwd'],
    ['climbing mid-path', 'src/../../secret'],
    ['a dot component', 'src/./lib.rs'],
    ['an empty component', 'src//lib.rs'],
    ['an embedded NUL', 'src/lib\0.rs'],
  ])('%s is refused', (_label, path) => {
    expect(() => sharePathParts(path)).toThrow(/not a valid path/)
  })

  test('a failure carries the bad_argument code', () => {
    expect(() => sharePathParts('../x')).toThrow(ToolInputError)
    try {
      sharePathParts('../x')
    } catch (error) {
      expect((error as ToolInputError).code).toBe('bad_argument')
    }
  })
})

describe('result shapes', () => {
  test('a success is flat, so a model does not have to unwrap it', () => {
    expect(ok({ ticket: 'abc', files: 2 })).toEqual({ ok: true, ticket: 'abc', files: 2 })
  })

  test('a failure carries both a code and prose', () => {
    expect(fail('unauthorized', 'needs a password')).toEqual({
      ok: false,
      code: 'unauthorized',
      error: 'needs a password',
    })
  })
})
