import { describe, expect, test } from 'bun:test'

import { mountFolderCandidates, shareStamp } from './index.ts'

// Local time, so the parts are built from the same fields `shareStamp` reads.
// Deliberately single-digit month/day/hour/minute/second: that is the padding
// path, and an unpadded stamp is the failure this file exists to catch — it
// sorts wrong and stops matching the CLI's.
const SINGLE_DIGITS = new Date(2026, 0, 2, 3, 4, 5)
const TWO_DIGITS = new Date(2026, 10, 25, 13, 45, 59)

describe('shareStamp', () => {
  test('is the documented agent-share-YYYY-MM-DDTHHMM shape', () => {
    expect(shareStamp(TWO_DIGITS)).toBe('agent-share-2026-11-25T1345')
    expect(shareStamp(TWO_DIGITS)).toMatch(/^agent-share-\d{4}-\d{2}-\d{2}T\d{4}$/)
  })

  test('pads every single-digit field', () => {
    expect(shareStamp(SINGLE_DIGITS)).toBe('agent-share-2026-01-02T0304')
  })

  test('seconds append exactly two more digits', () => {
    expect(shareStamp(TWO_DIGITS, true)).toBe(`${shareStamp(TWO_DIGITS)}59`)
    expect(shareStamp(SINGLE_DIGITS, true)).toBe(`${shareStamp(SINGLE_DIGITS)}05`)
  })
})

describe('mountFolderCandidates', () => {
  // The two share one definition of the format. Pinned because they are used
  // for different artifacts — a mount folder and a download — and a caller
  // reading one would reasonably assume the other matches.
  test('starts at the bare stamp, then adds seconds', () => {
    const names = mountFolderCandidates(TWO_DIGITS)
    expect(names[0]).toBe(shareStamp(TWO_DIGITS))
    expect(names[1]).toBe(shareStamp(TWO_DIGITS, true))
  })

  test('retries with a numeric suffix on the seconds form', () => {
    const names = mountFolderCandidates(TWO_DIGITS)
    expect(names[2]).toBe(`${shareStamp(TWO_DIGITS, true)}-2`)
    expect(names.at(-1)).toBe(`${shareStamp(TWO_DIGITS, true)}-99`)
    expect(names).toHaveLength(100)
  })
})
