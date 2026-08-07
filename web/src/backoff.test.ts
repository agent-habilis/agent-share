import { describe, expect, test } from 'bun:test'

import { REVIVAL_ORIGIN_CAP_MS, jittered, revivalOriginCapMs } from './backoff.ts'

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

describe('revivalOriginCapMs', () => {
  test('starts tight, because a connection that just died means a dead origin', () => {
    expect(revivalOriginCapMs(0)).toBe(REVIVAL_ORIGIN_CAP_MS)
  })

  /**
   * The bug: every attempt passed the same 8 s, so a producer that came back
   * on a slow link — the measured cold dial runs 6-20 s — was cut off on
   * every single attempt, forever. The tab kept reading frozen seeder
   * snapshots for as long as it stayed open.
   */
  test('stops capping, so a slow producer coming back is eventually reachable', () => {
    const caps = Array.from({ length: 50 }, (_, attempt) => revivalOriginCapMs(attempt))
    expect(caps.some((cap) => cap === undefined)).toBe(true)
    // And once it lets go, it stays let go: no attempt past the first
    // uncapped one re-imposes the tight bet.
    const firstUncapped = caps.indexOf(undefined)
    expect(caps.slice(firstUncapped).every((cap) => cap === undefined)).toBe(true)
  })

  test('a cap it does hand out clears nothing shorter than the tight bet', () => {
    for (let attempt = 0; attempt < 50; attempt++) {
      const cap = revivalOriginCapMs(attempt)
      if (cap !== undefined) expect(cap).toBeGreaterThanOrEqual(REVIVAL_ORIGIN_CAP_MS)
    }
  })
})
