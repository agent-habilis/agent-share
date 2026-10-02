/**
 * A share listing built from picked `File`s instead of a directory handle.
 *
 * `showDirectoryPicker()` exists only in Chromium. Everywhere else the way to
 * get real file bytes is `<input type="file">`, which hands back `File`s — and
 * a `File` is already a range-readable view of the file on disk, so the wasm
 * producer serves one without copying a byte. What is missing is the shape:
 * the picker walks a tree and yields relative paths, while an input yields a
 * flat list whose only structure is `webkitRelativePath`.
 *
 * This module rebuilds that shape. It is pure and takes a structural type
 * rather than `File`, so it can be tested with plain objects.
 *
 * The result is a **snapshot**. There is no rescan: re-reading the folder needs
 * a fresh user gesture, so what was picked is what gets served for the life of
 * the share.
 */

import { safeSplit } from '../tree.ts'

/** The part of `File` this module reads. */
export interface PickedFile {
  readonly name: string
  readonly size: number
  readonly lastModified: number
  readonly webkitRelativePath: string
}

export interface SnapshotListing<T> {
  dirs: string[]
  files: { rel_path: string; size: number; mtime: number; source: T }[]
  /**
   * Files dropped because their path could escape the share root. The wasm side
   * drops them too, silently; counting them here is what lets the UI admit that
   * the share is not everything the user picked.
   */
  skipped: number
  /** Files whose path collided and were served under a suffixed name. */
  renamed: number
}

/**
 * The path the file was picked under, with the picked folder's own name gone.
 *
 * `webkitRelativePath` is `picked/a/b.txt`; `scanDirectory` walks from the root
 * with an empty prefix and so produces `a/b.txt`. Stripping one segment is what
 * makes the two pickers describe the same tree.
 *
 * Only when every entry agrees on that first segment — a disagreement means
 * this is not a single picked folder, and mangling it would be worse than
 * leaving it alone.
 */
function stripPickedRoot(paths: string[]): string[] {
  const cut = paths[0]?.indexOf('/') ?? -1
  if (cut < 0) return paths
  const prefix = paths[0]!.slice(0, cut + 1)
  return paths.every((path) => path.startsWith(prefix))
    ? paths.map((path) => path.slice(prefix.length))
    : paths
}

/** `a/x.txt` → `a/x (2).txt`, keeping the extension where a reader expects it. */
function suffixed(relPath: string, attempt: number): string {
  const cut = relPath.lastIndexOf('/')
  const dir = cut === -1 ? '' : relPath.slice(0, cut + 1)
  const name = relPath.slice(cut + 1)
  const dot = name.lastIndexOf('.')
  return dot > 0
    ? `${dir}${name.slice(0, dot)} (${attempt})${name.slice(dot)}`
    : `${dir}${name} (${attempt})`
}

export function snapshotListing<T extends PickedFile>(
  picked: readonly T[],
): SnapshotListing<T> {
  // `webkitRelativePath` is empty for a loose-file pick, and for a folder pick
  // on iOS Safari, which offers no folder selection however the input is set up.
  const paths = stripPickedRoot(picked.map((file) => file.webkitRelativePath || file.name))

  const files: SnapshotListing<T>['files'] = []
  const taken = new Set<string>()
  const dirs = new Set<string>()
  // Where the next suffix for a colliding path starts, so resolving the k-th
  // duplicate does not rescan the k-1 before it. A camera roll full of
  // `IMG_0001.jpg` is the case that makes the difference.
  const nextAttempt = new Map<string, number>()
  let renamed = 0

  picked.forEach((file, index) => {
    const relPath = paths[index] as string
    const parts = safeSplit(relPath)
    if (!parts) return

    // Two folders can each hold an `x.txt`, and a flat pick flattens them onto
    // one path. The producer resolves duplicates first-wins, so without a
    // rename here the second file is dropped and nobody is told.
    let path = relPath
    let attempt = nextAttempt.get(relPath) ?? 2
    while (taken.has(path)) {
      path = suffixed(relPath, attempt)
      attempt += 1
    }
    nextAttempt.set(relPath, attempt)
    if (path !== relPath) renamed += 1
    taken.add(path)

    // Every prefix, because an input reports files only: a directory exists
    // here just as far as something under it does.
    let prefix = ''
    for (const part of parts.slice(0, -1)) {
      prefix = prefix ? `${prefix}/${part}` : part
      dirs.add(prefix)
    }

    files.push({
      rel_path: path,
      size: file.size,
      mtime: Math.floor(file.lastModified / 1000),
      source: file,
    })
  })

  // Derived, not tracked: everything dropped was refused by `safeSplit`.
  return { dirs: [...dirs].sort(), files, skipped: picked.length - files.length, renamed }
}
