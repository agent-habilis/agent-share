/**
 * A file input reports a flat list; a share needs a tree. Everything here is
 * about the gap between the two — and about the two ways picked files quietly
 * disappear if nobody counts them, an unsafe path and a name collision.
 */

import { test, expect } from 'bun:test'

import { snapshotListing, type PickedFile } from './index.ts'

function picked(webkitRelativePath: string, overrides: Partial<PickedFile> = {}): PickedFile {
  const name = webkitRelativePath.slice(webkitRelativePath.lastIndexOf('/') + 1)
  return { name, size: 1, lastModified: 0, webkitRelativePath, ...overrides }
}

/** A loose-file pick: no `webkitRelativePath`, so the name is the whole path. */
function loose(name: string, overrides: Partial<PickedFile> = {}): PickedFile {
  return { name, size: 1, lastModified: 0, webkitRelativePath: '', ...overrides }
}

test('drops the picked folder from every path', () => {
  const listing = snapshotListing([picked('shots/a.txt'), picked('shots/deep/b.txt')])
  expect(listing.files.map((file) => file.rel_path)).toEqual(['a.txt', 'deep/b.txt'])
})

test('keeps paths whole when they do not agree on a root', () => {
  // Not one picked folder, so there is no root to drop and guessing one would
  // silently move half the files up a level.
  const listing = snapshotListing([picked('shots/a.txt'), picked('other/b.txt')])
  expect(listing.files.map((file) => file.rel_path)).toEqual(['shots/a.txt', 'other/b.txt'])
})

test('falls back to the name when there is no relative path', () => {
  const listing = snapshotListing([loose('a.txt'), loose('b.txt')])
  expect(listing.files.map((file) => file.rel_path)).toEqual(['a.txt', 'b.txt'])
  expect(listing.dirs).toEqual([])
})

test('derives every directory from the paths under it', () => {
  const listing = snapshotListing([
    picked('shots/one/two/deep.txt'),
    picked('shots/one/flat.txt'),
  ])
  expect(listing.dirs).toEqual(['one', 'one/two'])
})

test('counts and drops paths that could escape the root', () => {
  const listing = snapshotListing([
    picked('shots/ok.txt'),
    loose('../escape.txt'),
    loose('/absolute.txt'),
    loose('back\\slash.txt'),
    loose('./dot.txt'),
  ])
  expect(listing.files.map((file) => file.rel_path)).toEqual(['shots/ok.txt'])
  expect(listing.skipped).toBe(4)
})

test('renames a colliding path instead of losing the file', () => {
  // The producer resolves duplicates first-wins, so an unrenamed second file is
  // dropped with nothing said.
  const listing = snapshotListing([loose('x.txt'), loose('x.txt'), loose('x.txt')])
  expect(listing.files.map((file) => file.rel_path)).toEqual([
    'x.txt',
    'x (2).txt',
    'x (3).txt',
  ])
  expect(listing.renamed).toBe(2)
})

test('suffixes an extensionless name at the end', () => {
  const listing = snapshotListing([loose('README'), loose('README')])
  expect(listing.files[1]?.rel_path).toBe('README (2)')
})

test('carries size and seconds-resolution mtime through', () => {
  const listing = snapshotListing([loose('a.bin', { size: 4096, lastModified: 1_700_000_123_456 })])
  expect(listing.files[0]?.size).toBe(4096)
  expect(listing.files[0]?.mtime).toBe(1_700_000_123)
})

test('keeps a zero-byte file, which is a slot with nothing to read', () => {
  const listing = snapshotListing([loose('empty.txt', { size: 0 })])
  expect(listing.files).toHaveLength(1)
  expect(listing.files[0]?.size).toBe(0)
})

test('hands the picked file straight back as the source', () => {
  const file = loose('a.txt')
  expect(snapshotListing([file]).files[0]?.source).toBe(file)
})

test('an empty pick is an empty listing, not a throw', () => {
  expect(snapshotListing([])).toEqual({ dirs: [], files: [], skipped: 0, renamed: 0 })
})
