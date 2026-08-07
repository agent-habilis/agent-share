import { describe, expect, test } from 'bun:test'

import { jittered } from './backoff.ts'

describe('jittered', () => {
  test('stays within [0.5x, 1.5x) of the requested delay', () => {
    for (let i = 0; i < 1_000; i++) {
      const delay = jittered(1_000)
      expect(delay).toBeGreaterThanOrEqual(500)
      expect(delay).toBeLessThan(1_500)
    }
  })

  test('actually spreads: a volley of retries does not stay in lockstep', () => {
    const volley = new Set(Array.from({ length: 100 }, () => jittered(30_000)))
    // 100 identical draws would mean the jitter is not jittering; even a
    // handful of distinct values proves the volley de-phased.
    expect(volley.size).toBeGreaterThan(10)
  })
})
