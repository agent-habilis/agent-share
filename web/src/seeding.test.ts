import { describe, expect, test } from 'bun:test'

import {
  dirSeedState,
  fileSeedState,
  missingUnder,
  seedLabel,
  shareSeedSummary,
} from './seeding.ts'
import type { DirNode, FileNode } from './tree.ts'

function file(name: string, index: number): FileNode {
  return {
    kind: 'file',
    name,
    path: name,
    size: 10,
    mtime: 0,
    index,
  }
}

function dir(name: string, children: (DirNode | FileNode)[]): DirNode {
  return { kind: 'dir', name, path: name, children }
}

describe('fileSeedState', () => {
  test('a held index is seeding', () => {
    expect(fileSeedState(file('a', 3), new Set([3]))).toBe('full')
  })

  test('an unheld index is not', () => {
    expect(fileSeedState(file('a', 3), new Set([1, 2]))).toBe('none')
  })
})

describe('dirSeedState', () => {
  const tree = dir('docs', [file('a', 0), file('b', 1), dir('deep', [file('c', 2)])])

  test('every file held is full', () => {
    expect(dirSeedState(tree, new Set([0, 1, 2]))).toBe('full')
  })

  test('no file held is none', () => {
    expect(dirSeedState(tree, new Set([9]))).toBe('none')
  })

  // Rounding either way misleads: down hides real progress, up claims files
  // that were never fetched. Neither helps someone decide whether to sync.
  test('some files held is its own state', () => {
    expect(dirSeedState(tree, new Set([0, 2]))).toBe('partial')
  })

  test('an empty folder holds nothing', () => {
    expect(dirSeedState(dir('empty', []), new Set([0]))).toBe('none')
  })
})

describe('missingUnder', () => {
  test('names only what is absent', () => {
    const tree = dir('docs', [file('a', 0), file('b', 1)])
    expect(missingUnder(tree, new Set([0])).map((f) => f.index)).toEqual([1])
  })

  test('a held file is missing nothing', () => {
    expect(missingUnder(file('a', 4), new Set([4]))).toEqual([])
  })
})

describe('seedLabel', () => {
  test('a held file reads as seeding, not as downloaded', () => {
    expect(seedLabel('full', file('a', 0), new Set([0]))).toBe('seeding')
  })

  test('a partial folder counts', () => {
    const tree = dir('docs', [file('a', 0), file('b', 1), file('c', 2)])
    expect(seedLabel('partial', tree, new Set([0, 2]))).toBe('seeding 2 of 3')
  })
})

describe('shareSeedSummary', () => {
  test('counts across the whole tree', () => {
    const root = dir('', [file('a', 0), dir('d', [file('b', 1), file('c', 2)])])
    expect(shareSeedSummary(root, new Set([0, 1]))).toEqual({
      held: 2,
      total: 3,
      state: 'partial',
    })
  })

  test('a share holding nothing is none', () => {
    const root = dir('', [file('a', 0)])
    expect(shareSeedSummary(root, new Set()).state).toBe('none')
  })
})
