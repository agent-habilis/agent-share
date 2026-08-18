import { describe, expect, test } from 'bun:test'

import { shareProgress } from './index.ts'
import type { FileNode } from 'agent-share-core/tree'

function file(index: number, size: number): FileNode {
  return { kind: 'file', name: `f${index}`, path: `f${index}`, index, size, mtime: 0 }
}

const big = file(0, 4_000_000_000)
const small = [file(1, 100), file(2, 100), file(3, 100)]

describe('shareProgress', () => {
  test('an empty share is 0, not NaN', () => {
    const progress = shareProgress([], new Set(), new Map())
    expect(progress.fraction).toBe(0)
    expect(progress.bytesTotal).toBe(0)
  })

  // The reason this is byte-weighted at all: three of four files held is 75%
  // of the slots and effectively none of the share.
  test('weights by bytes, not by file count', () => {
    const progress = shareProgress([big, ...small], new Set([1, 2, 3]), new Map())
    expect(progress.filesComplete).toBe(3)
    expect(progress.fraction).toBeLessThan(0.001)
  })

  test('a partial file counts for its fraction of itself', () => {
    const progress = shareProgress([file(0, 1000)], new Set(), new Map([[0, 0.25]]))
    expect(progress.bytesHeld).toBe(250)
    expect(progress.fraction).toBe(0.25)
    expect(progress.filesComplete).toBe(0)
  })

  test('a full coverage entry counts as complete even without a held slot', () => {
    const progress = shareProgress([file(0, 1000)], new Set(), new Map([[0, 1]]))
    expect(progress.fraction).toBe(1)
    expect(progress.filesComplete).toBe(1)
  })

  test('over-reported coverage cannot push the bar past full', () => {
    const progress = shareProgress([file(0, 1000)], new Set(), new Map([[0, 1.5]]))
    expect(progress.fraction).toBe(1)
  })

  test('everything held is exactly full', () => {
    const files = [big, ...small]
    const progress = shareProgress(files, new Set([0, 1, 2, 3]), new Map())
    expect(progress.fraction).toBe(1)
    expect(progress.bytesHeld).toBe(progress.bytesTotal)
    expect(progress.filesComplete).toBe(4)
  })
})
