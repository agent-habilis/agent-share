/**
 * A share root that needs no user gesture.
 *
 * `showDirectoryPicker()` is how a person shares a folder, and it requires a
 * real click — which leaves anything automated with no way to produce at all.
 * The origin private file system is the same File System Access API without the
 * picker: `navigator.storage.getDirectory()` returns a real
 * `FileSystemDirectoryHandle` and `getFileHandle(…, {create: true})` a real
 * `FileSystemFileHandle`. That matters beyond convenience, because the wasm
 * side checks the type — `parse_listing` does a
 * `dyn_into::<FileSystemFileHandle>()` — so an object that merely has
 * `getFile()` is refused.
 *
 * What comes out is an ordinary handle, so `startProducer` serves it exactly as
 * it serves a picked folder. Only the origin of the handle differs.
 *
 * The bytes live in the browser's per-origin storage. They are subject to the
 * origin's quota and to eviction under pressure, and callers should not promise
 * anyone that a share built this way outlives the tab serving it.
 */

import { safeSplit } from '../tree.ts'

export interface OpfsDirectory {
  handle: FileSystemDirectoryHandle
  /** The name under the OPFS root, so it can be removed again. */
  name: string
}

export function canUseOpfs(): boolean {
  return typeof navigator.storage?.getDirectory === 'function'
}

/**
 * A fresh, uniquely named directory under the OPFS root.
 *
 * Unique per call on purpose. OPFS is per-origin and survives a reload, so a
 * fixed name would accumulate files across runs and quietly change what a later
 * share serves.
 */
export async function createShareDirectory(prefix = 'share'): Promise<OpfsDirectory> {
  if (!canUseOpfs()) {
    throw new Error('this browser has no origin private file system')
  }
  const opfs = await navigator.storage.getDirectory()
  const name = `${prefix}-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`
  const handle = await opfs.getDirectoryHandle(name, { create: true })
  return { handle, name }
}

/**
 * Write `contents` to `relPath` under `dir`, creating parent directories.
 *
 * The path is validated the way a manifest path is: no absolute paths, no `.`
 * or `..`, no backslashes. Here it is not a formality — these components become
 * real directory entries, and a caller handing us a path is often relaying one
 * from somewhere else.
 */
export async function writeOpfsFile(
  dir: FileSystemDirectoryHandle,
  relPath: string,
  contents: string | Uint8Array<ArrayBuffer>,
): Promise<void> {
  const parts = safeSplit(relPath)
  if (!parts || parts.length === 0) {
    throw new Error(`"${relPath}" is not a valid path inside the share`)
  }
  const name = parts[parts.length - 1] as string

  let parent = dir
  for (const segment of parts.slice(0, -1)) {
    parent = await parent.getDirectoryHandle(segment, { create: true })
  }

  const file = await parent.getFileHandle(name, { create: true })
  if (typeof file.createWritable !== 'function') {
    throw new Error('this browser cannot write to the origin private file system')
  }
  const writable = await file.createWritable()
  await writable.write(contents)
  await writable.close()
}

/**
 * Remove a directory made by [`createShareDirectory`], best effort.
 *
 * Best effort because a directory left behind costs disk, not correctness, and
 * failing a teardown over it would be the worse trade.
 */
export async function removeShareDirectory(name: string): Promise<boolean> {
  try {
    const opfs = await navigator.storage.getDirectory()
    await opfs.removeEntry(name, { recursive: true })
    return true
  } catch {
    return false
  }
}
