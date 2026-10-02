import { describe, expect, test } from 'bun:test'

import { globToRegExp, pathFilter } from './glob.ts'

describe('glob patterns', () => {
  test('a single star stops at a directory separator', () => {
    const regex = globToRegExp('*.rs')

    expect(regex.test('lib.rs')).toBe(true)
    expect(regex.test('src/lib.rs')).toBe(false)
  })

  test('a double star crosses separators', () => {
    const regex = globToRegExp('src/**/*.rs')

    expect(regex.test('src/mount/nfs.rs')).toBe(true)
    expect(regex.test('src/deep/deeper/x.rs')).toBe(true)
  })

  // `**/` has to mean "zero or more directories", or `**/x` misses a bare `x`.
  test('a leading double star matches nothing at all', () => {
    expect(globToRegExp('**/lib.rs').test('lib.rs')).toBe(true)
    expect(globToRegExp('**/lib.rs').test('src/deep/lib.rs')).toBe(true)
  })

  test('a question mark is exactly one character, not a separator', () => {
    expect(globToRegExp('a?.txt').test('ab.txt')).toBe(true)
    expect(globToRegExp('a?.txt').test('abc.txt')).toBe(false)
    expect(globToRegExp('a?b').test('a/b')).toBe(false)
  })

  test('regex metacharacters in a pattern are literal', () => {
    expect(globToRegExp('v1.2+beta(x).txt').test('v1.2+beta(x).txt')).toBe(true)
    // The dot would match anything if it were not escaped.
    expect(globToRegExp('a.txt').test('axtxt')).toBe(false)
  })

  test('a pattern is anchored at both ends', () => {
    expect(globToRegExp('lib.rs').test('mylib.rs')).toBe(false)
    expect(globToRegExp('lib.rs').test('lib.rs.bak')).toBe(false)
  })
})

describe('choosing what a pattern is matched against', () => {
  test('no pattern takes everything', () => {
    const filter = pathFilter(undefined)

    expect(filter('anything/at/all.bin')).toBe(true)
  })

  // The whole reason `*.rs` does the obvious thing.
  test('a pattern without a slash matches the file name', () => {
    const filter = pathFilter('*.rs')

    expect(filter('lib.rs')).toBe(true)
    expect(filter('crates/agent-share/src/lib.rs')).toBe(true)
    expect(filter('crates/lib.rs.txt')).toBe(false)
  })

  test('a pattern with a slash matches the whole path', () => {
    const filter = pathFilter('src/**')

    expect(filter('src/lib.rs')).toBe(true)
    expect(filter('src/deep/lib.rs')).toBe(true)
    expect(filter('web/src/lib.rs')).toBe(false)
  })

  test('an empty pattern is treated as no pattern', () => {
    expect(pathFilter('')('whatever.txt')).toBe(true)
  })
})
