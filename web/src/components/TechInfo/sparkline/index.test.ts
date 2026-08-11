import { describe, expect, test } from 'bun:test'

import { sparkline } from './index.ts'

describe('sparkline', () => {
  test('is always exactly the requested width', () => {
    expect(sparkline([], 10)).toHaveLength(10)
    expect(sparkline([1, 2, 3], 10)).toHaveLength(10)
    expect(sparkline(Array.from({ length: 200 }, (_, i) => i), 10)).toHaveLength(10)
  })

  test('grows in from the right', () => {
    expect(sparkline([5, 10], 5)).toBe('   ▄█')
  })

  test('keeps the newest samples when history is longer than the row', () => {
    expect(sparkline([9, 9, 9, 1, 2], 2)).toBe('▄█')
  })

  // The distinction the ramp exists for: a tick with no bytes is blank, and
  // the slowest tick that did move still gets a mark.
  test('nothing moved is blank, a trickle is not', () => {
    expect(sparkline([0, 0, 0], 3)).toBe('   ')
    expect(sparkline([0, 1, 1000], 3)).toBe(' ▁█')
  })

  test('scales to the window rather than to all history', () => {
    // The 1000 falls outside the window, so 10 is the peak that 5 is drawn
    // against — not a sliver against a number the row cannot show.
    expect(sparkline([1000, 5, 10], 2)).toBe('▄█')
  })

  test('a flat non-zero line is full height', () => {
    expect(sparkline([7, 7, 7], 3)).toBe('███')
  })

  test('a zero-width row is empty', () => {
    expect(sparkline([1, 2, 3], 0)).toBe('')
  })
})
