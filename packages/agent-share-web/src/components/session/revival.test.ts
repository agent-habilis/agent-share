import { expect, test } from 'bun:test'

import { describeRevival } from './revival.ts'

test('the first attempt says only that the connection dropped', () => {
  expect(describeRevival({ attempts: 0, lastError: null })).toBe(
    'Reconnecting — the connection dropped while the tab was away.',
  )
})

test('failed attempts carry their count and the last error', () => {
  expect(describeRevival({ attempts: 1, lastError: 'origin dial timed out after 8 s' })).toBe(
    'Reconnecting — 1 attempt failed so far.\nLast error: origin dial timed out after 8 s',
  )
  expect(describeRevival({ attempts: 4, lastError: null })).toBe(
    'Reconnecting — 4 attempts failed so far.',
  )
})
