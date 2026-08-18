import { describe, expect, test } from 'bun:test'

import { dirSeedState, fileSeedState, missingUnder, seedLabel } from './index.ts'
import type { DirNode, FileNode } from '../tree.ts'

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
  // that were never fetched. Neither helps someone decide whether to seed.
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

describe('partial holdings', () => {
  // A file can be `partial` now, which it never could before: chunks are
  // addressed and verified one at a time, so a peer holding part of a file
  // serves that part. These cases pin the states a cancelled download, an
  // abandoned preview and a transfer in flight all land in.

  test('a partly-held file reads as partial, not as absent', () => {
    const root = dir('', [file('movie.mkv', 0)])
    const target = root.children[0] as FileNode
    const coverage = new Map([[0, 0.6]])
    expect(fileSeedState(target, new Set(), coverage)).toBe('partial')
    expect(seedLabel('partial', target, new Set(), coverage)).toBe('seeding 60%')
  })

  test('a fraction that rounds to zero still reads as seeding, not as nothing', () => {
    // The one number that would make a genuinely useful source look useless.
    const root = dir('', [file('huge.bin', 0)])
    const target = root.children[0] as FileNode
    const coverage = new Map([[0, 0.0001]])
    expect(fileSeedState(target, new Set(), coverage)).toBe('partial')
    expect(seedLabel('partial', target, new Set(), coverage)).toBe('seeding 1%')
  })

  test('a file with no coverage entry reads as nothing, never as full', () => {
    // The contract `coverage_map` has to produce. It asks the store per row,
    // and an empty coverage answers `1.0` to `fraction()` — so a row the store
    // has no map for once arrived here as "fully seeded". Absent means unknown,
    // and unknown must paint as nothing rather than as a promise.
    const root = dir('', [file('never-fetched.bin', 0)])
    const target = root.children[0] as FileNode
    expect(fileSeedState(target, new Set(), new Map())).toBe('none')
    expect(fileSeedState(target, new Set(), new Map([[0, 0]]))).toBe('none')
  })

  test('a fully-covered file reads as full even before the held set catches up', () => {
    // `held` is refreshed synchronously and coverage lands a tick later, so the
    // two disagree briefly. Neither ordering may report less than is held.
    const root = dir('', [file('done.bin', 0)])
    const target = root.children[0] as FileNode
    expect(fileSeedState(target, new Set(), new Map([[0, 1]]))).toBe('full')
    expect(fileSeedState(target, new Set([0]), new Map())).toBe('full')
  })

  test('a folder holding part of one file is partial rather than empty', () => {
    const root = dir('', [file('a.bin', 0), file('b.bin', 1)])
    const coverage = new Map([[0, 0.5]])
    expect(dirSeedState(root, new Set(), coverage)).toBe('partial')
    expect(dirSeedState(root, new Set(), new Map())).toBe('none')
  })

  test('a folder is only full when every file is', () => {
    const root = dir('', [file('a.bin', 0), file('b.bin', 1)])
    expect(dirSeedState(root, new Set([0, 1]), new Map())).toBe('full')
    expect(dirSeedState(root, new Set([0]), new Map([[1, 0.9]]))).toBe('partial')
  })

  test('no coverage means the old behaviour, exactly', () => {
    // Every existing caller passes nothing, so the default must not change what
    // they see.
    const root = dir('', [file('a.bin', 0)])
    const target = root.children[0] as FileNode
    expect(fileSeedState(target, new Set())).toBe('none')
    expect(fileSeedState(target, new Set([0]))).toBe('full')
    expect(seedLabel('none', target, new Set())).toBe('not held')
  })
})
