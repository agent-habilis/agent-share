/**
 * The totals `buildTree` hangs off every directory.
 *
 * They exist so a caller never flattens a subtree to ask how big it is, which
 * means the only thing keeping them honest is that they agree with `filesUnder`
 * — the walk they replaced. That agreement is what these pin, on the two cases
 * where a tally taken from the manifest instead would get it wrong: a tombstone,
 * and a path that escapes the root.
 */

import { describe, expect, test } from 'bun:test'

import { buildTree, dirNode, filesUnder, type DirNode, type Manifest } from './tree.ts'

function manifest(files: Manifest['files']): Manifest {
  return { dirs: [{ rel_path: 'src', mode: 0o755, mtime: 0 }], files }
}

const file = (rel_path: string, size: number) => ({ rel_path, size, mode: 0o644, mtime: 0 })

function totalsAgree(root: DirNode): void {
  const walked = filesUnder(root)
  expect(root.files).toBe(walked.length)
  expect(root.bytes).toBe(walked.reduce((sum, entry) => sum + entry.size, 0))
}

describe('directory totals', () => {
  test('a directory counts everything beneath it, not just its own children', () => {
    const { root } = buildTree(manifest([file('top.txt', 1), file('src/deep.rs', 20)]))

    expect(root.files).toBe(2)
    expect(root.bytes).toBe(21)
    expect(root.dirs).toBe(1)
    totalsAgree(root)
  })

  test('an empty share totals zero rather than going undefined', () => {
    const { root } = buildTree({ dirs: [], files: [] })

    expect([root.files, root.bytes, root.dirs]).toEqual([0, 0, 0])
  })

  test('a tombstone is not counted — its slot stays, its bytes do not', () => {
    const { root } = buildTree(manifest([file('kept.txt', 5), file('', 0)]))

    expect(root.files).toBe(1)
    totalsAgree(root)
  })

  /**
   * The reason `connect` reports these rather than tallying the manifest: a
   * hostile path is dropped from the tree but still sits in `manifest.files`,
   * so the two would disagree about the same share.
   */
  test('a path that escapes the root is skipped by the totals too', () => {
    const { root, skipped } = buildTree(manifest([file('fine.txt', 5), file('../escape', 999)]))

    expect(skipped).toBe(1)
    expect(root.files).toBe(1)
    expect(root.bytes).toBe(5)
    totalsAgree(root)
  })

  test('dirNode totals a hand-built tree the same way', () => {
    const built = dirNode('', '', [
      { kind: 'file', name: 'a.txt', path: 'a.txt', index: 0, size: 3, mtime: 0 },
      dirNode('sub', 'sub', [
        { kind: 'file', name: 'b.txt', path: 'sub/b.txt', index: 1, size: 4, mtime: 0 },
      ]),
    ])

    expect([built.files, built.bytes, built.dirs]).toEqual([2, 7, 1])
    totalsAgree(built)
  })
})
