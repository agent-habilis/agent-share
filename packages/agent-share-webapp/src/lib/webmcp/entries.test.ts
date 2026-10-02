import { describe, expect, test } from 'bun:test'

import { buildTree, type Manifest } from '../tree.ts'
import { collect, describeEntry, locate, requireFile, type Entry } from './entries.ts'
import { ToolInputError } from './result.ts'

function manifest(): Manifest {
  return {
    dirs: [
      { rel_path: 'src', mode: 0o755, mtime: 10 },
      { rel_path: 'src/deep', mode: 0o755, mtime: 11 },
      { rel_path: 'empty', mode: 0o755, mtime: 12 },
    ],
    files: [
      { rel_path: 'README.md', size: 120, mode: 0o644, mtime: 1 },
      { rel_path: 'src/lib.rs', size: 4096, mode: 0o644, mtime: 2 },
      { rel_path: 'src/deep/nested.rs', size: 64, mode: 0o644, mtime: 3 },
    ],
  }
}

const tree = () => buildTree(manifest()).root

describe('locating a path', () => {
  test('an empty path is the share root', () => {
    expect(locate(tree(), []).kind).toBe('dir')
  })

  test('a file is found at its own path', () => {
    const node = locate(tree(), ['src', 'lib.rs'])

    expect(node).toMatchObject({ kind: 'file', path: 'src/lib.rs', size: 4096 })
  })

  test('a missing path names itself in the failure', () => {
    expect(() => locate(tree(), ['src', 'nope.rs'])).toThrow(/"src\/nope\.rs" is not in this share/)
  })

  test('a missing path carries the not_found code', () => {
    try {
      locate(tree(), ['nope'])
      throw new Error('should have thrown')
    } catch (error) {
      expect(error).toBeInstanceOf(ToolInputError)
      expect((error as ToolInputError).code).toBe('not_found')
    }
  })

  test('requireFile refuses a directory, and says which one', () => {
    expect(() => requireFile(tree(), ['src'])).toThrow(/"src" is a directory, not a file/)
  })

  test('requireFile returns the manifest index a read is addressed by', () => {
    expect(requireFile(tree(), ['README.md']).index).toBe(0)
    expect(requireFile(tree(), ['src', 'deep', 'nested.rs']).index).toBe(2)
  })
})

describe('describing entries', () => {
  test('a file reports raw numbers, not formatted sizes', () => {
    const entry = describeEntry(locate(tree(), ['src', 'lib.rs']))

    expect(entry).toEqual({
      kind: 'file',
      path: 'src/lib.rs',
      name: 'lib.rs',
      size: 4096,
      mtime: 2,
    })
    expect(typeof entry.size).toBe('number')
  })

  test('a directory reports how many entries are directly inside', () => {
    expect(describeEntry(locate(tree(), ['src']))).toEqual({
      kind: 'dir',
      path: 'src',
      name: 'src',
      entries: 2,
    })
  })

  test('an empty directory is still an entry', () => {
    expect(describeEntry(locate(tree(), ['empty']))).toMatchObject({ kind: 'dir', entries: 0 })
  })
})

describe('collecting a listing', () => {
  function listing(path: string[], depth: number): Entry[] {
    const into: Entry[] = []
    const node = locate(tree(), path)
    if (node.kind !== 'dir') throw new Error('expected a directory')
    collect(node, depth, into)
    return into
  }

  test('depth 1 is the immediate children only', () => {
    expect(listing([], 1).map((entry) => entry.path).sort()).toEqual(['README.md', 'empty', 'src'])
  })

  test('a deeper listing includes the nested entries', () => {
    expect(listing([], 2).map((entry) => entry.path).sort()).toEqual([
      'README.md',
      'empty',
      'src',
      'src/deep',
      'src/lib.rs',
    ])
  })

  test('depth beyond the tree simply ends', () => {
    expect(listing([], 99).map((entry) => entry.path)).toContain('src/deep/nested.rs')
  })

  test('directories come before files, as the tree sorts them', () => {
    expect(listing([], 1).map((entry) => entry.kind)).toEqual(['dir', 'dir', 'file'])
  })
})

/**
 * A tombstone is a file slot whose path is empty: the file is gone, but the
 * slot stays so every later index still addresses the file it always did.
 * It must not appear in a listing, and it must not shift anything either.
 */
test('a tombstoned slot is absent from the listing but does not renumber the rest', () => {
  const withHole: Manifest = {
    dirs: [],
    files: [
      { rel_path: 'kept.txt', size: 1, mode: 0o644, mtime: 1 },
      { rel_path: '', size: 0, mode: 0, mtime: 0 },
      { rel_path: 'after.txt', size: 2, mode: 0o644, mtime: 2 },
    ],
  }
  const built = buildTree(withHole)
  const entries: Entry[] = []
  collect(built.root, 1, entries)

  expect(entries.map((entry) => entry.path)).toEqual(['after.txt', 'kept.txt'])
  expect(requireFile(built.root, ['after.txt']).index).toBe(2)
  // A tombstone is not a hostile path, so it must not be counted as one.
  expect(built.skipped).toBe(0)
})
