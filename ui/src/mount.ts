/**
 * Mirror a share into a host directory via the File System Access API.
 *
 * One-way and read-only from the peer's point of view: local edits are
 * overwritten on the next sync. Matches the CLI NFS mount's semantics, with
 * the browser picking the mountpoint through `showDirectoryPicker`.
 */

import { safeSplit, type FileNode, type ManifestDir } from './tree.ts'
import type { Progress } from './download.ts'

/** The protocol's per-request ceiling (`MAX_READ_LEN`). */
const CHUNK = 256 * 1024

interface Reader {
  read(index: number, offset: bigint, len: number): Promise<Uint8Array>
}

/** What we last wrote for a path — used to skip unchanged files. */
export interface SyncedFile {
  size: number
  mtime: number
  index: number
}

export interface SyncedState {
  files: Map<string, SyncedFile>
  dirs: Set<string>
}

export function canMount(): boolean {
  return typeof window.showDirectoryPicker === 'function'
}

export class MountError extends Error {
  constructor(message: string) {
    super(message)
    this.name = 'MountError'
  }
}

/** True when the directory has no entries (CLI requires an empty mountpoint). */
async function isEmpty(root: FileSystemDirectoryHandle): Promise<boolean> {
  for await (const _ of root.keys()) return false
  return true
}

/**
 * Ask the user for a writable directory and refuse a non-empty one.
 *
 * AbortError from the picker is rethrown so the caller can treat cancel as a
 * no-op; everything else becomes a MountError.
 */
export async function pickMountRoot(): Promise<FileSystemDirectoryHandle> {
  const picker = window.showDirectoryPicker
  if (!picker) {
    throw new MountError('This browser cannot mount folders')
  }
  const root = await picker({ mode: 'readwrite' })
  if (!(await isEmpty(root))) {
    throw new MountError('Mount directory must be empty')
  }
  return root
}

export function emptySyncedState(): SyncedState {
  return { files: new Map(), dirs: new Set() }
}

/** Walk `parts` under `root`, creating directories as needed. */
async function ensureDir(
  root: FileSystemDirectoryHandle,
  parts: string[],
): Promise<FileSystemDirectoryHandle> {
  let current = root
  for (const part of parts) {
    current = await current.getDirectoryHandle(part, { create: true })
  }
  return current
}

async function writeFile(
  root: FileSystemDirectoryHandle,
  reader: Reader,
  file: FileNode,
  onBytes: (n: number) => void,
): Promise<void> {
  const parts = safeSplit(file.path)
  if (!parts || parts.length === 0) return
  const name = parts[parts.length - 1] as string
  const parent = await ensureDir(root, parts.slice(0, -1))
  const handle = await parent.getFileHandle(name, { create: true })
  const writable = await handle.createWritable()
  try {
    let offset = 0
    while (offset < file.size) {
      const want = Math.min(CHUNK, file.size - offset)
      const chunk = await reader.read(file.index, BigInt(offset), want)
      if (chunk.length === 0) break
      // Copy: wasm may hand back a view over SharedArrayBuffer-backed memory.
      await writable.write(new Uint8Array(chunk))
      offset += chunk.length
      onBytes(chunk.length)
    }
    // A short remote file still needs a correct local size.
    if (offset < file.size) await writable.truncate(offset)
  } finally {
    await writable.close()
  }
}

/** Remove `relPath` (file or directory) under `root`. Missing is fine. */
async function removePath(root: FileSystemDirectoryHandle, relPath: string): Promise<void> {
  const parts = safeSplit(relPath)
  if (!parts || parts.length === 0) return
  try {
    let parent = root
    for (const part of parts.slice(0, -1)) {
      parent = await parent.getDirectoryHandle(part)
    }
    const name = parts[parts.length - 1] as string
    await parent.removeEntry(name, { recursive: true })
  } catch (error) {
    // NotFoundError: already gone — nothing to do.
    if (error instanceof DOMException && error.name === 'NotFoundError') return
    throw error
  }
}

function fileKey(file: FileNode): SyncedFile {
  return { size: file.size, mtime: file.mtime, index: file.index }
}

function unchanged(prev: SyncedFile | undefined, next: SyncedFile): boolean {
  return (
    prev !== undefined &&
    prev.size === next.size &&
    prev.mtime === next.mtime &&
    prev.index === next.index
  )
}

/**
 * Bring `root` in line with the current tree.
 *
 * Only files that are new or whose size/mtime/index changed are rewritten.
 * Paths that left the manifest are deleted. Progress totals cover bytes of
 * files that will actually be written this pass.
 */
export async function syncMount(
  root: FileSystemDirectoryHandle,
  reader: Reader,
  files: FileNode[],
  dirs: ManifestDir[],
  previous: SyncedState,
  onProgress?: (progress: Progress) => void,
): Promise<SyncedState> {
  const nextFiles = new Map<string, SyncedFile>()
  const nextDirs = new Set<string>()
  const toWrite: FileNode[] = []

  for (const dir of dirs) {
    const parts = safeSplit(dir.rel_path)
    if (!parts) continue
    nextDirs.add(dir.rel_path)
  }

  for (const file of files) {
    const meta = fileKey(file)
    nextFiles.set(file.path, meta)
    if (!unchanged(previous.files.get(file.path), meta)) toWrite.push(file)
  }

  // Create directory scaffolding first so empty dirs land even with no files.
  for (const rel of nextDirs) {
    const parts = safeSplit(rel)
    if (!parts) continue
    await ensureDir(root, parts)
  }

  const total = toWrite.reduce((sum, file) => sum + file.size, 0)
  let done = 0
  onProgress?.({ done, total })

  for (const file of toWrite) {
    await writeFile(root, reader, file, (n) => {
      done += n
      onProgress?.({ done, total })
    })
  }

  // Delete files that disappeared, deepest paths first so parents can go next.
  const removedFiles = [...previous.files.keys()].filter((path) => !nextFiles.has(path))
  removedFiles.sort((a, b) => b.length - a.length)
  for (const path of removedFiles) await removePath(root, path)

  const removedDirs = [...previous.dirs].filter((path) => !nextDirs.has(path))
  removedDirs.sort((a, b) => b.length - a.length)
  for (const path of removedDirs) await removePath(root, path)

  return { files: nextFiles, dirs: nextDirs }
}
