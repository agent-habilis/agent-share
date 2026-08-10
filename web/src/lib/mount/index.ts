/**
 * Mirror a share into a host directory via the File System Access API.
 *
 * One-way and read-only from the peer's point of view: local edits are
 * overwritten on the next sync. Matches the CLI NFS mount's semantics, with
 * the browser picking a parent target and creating `agent-share-…/` under it.
 */

import { safeSplit, type FileNode, type ManifestDir } from '../tree.ts'
import type { Progress } from '../download/index.ts'

/** The protocol's per-request ceiling (`MAX_READ_LEN`). */
const CHUNK = 256 * 1024

interface Reader {
  read(index: number, offset: bigint, len: number): Promise<Uint8Array>
  /** Keep what was read, so mirroring to a folder also seeds. */
  keep?(index: number, offset: bigint, bytes: Uint8Array): Promise<void>
  /**
   * Whether the mount reaches the ticket's origin, or a seeder standing in
   * for it. Absent reads as `true` (origin semantics). Guard #2 hangs off
   * this: a short read from the origin means the file shrank, but from a
   * seeder it means "I cannot finish", and truncating on it would silently
   * corrupt the mirror.
   */
  readonly source_is_origin?: boolean
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

/** A live mount: sync root plus enough to remove the folder on unmount. */
export interface MountSession {
  root: FileSystemDirectoryHandle
  parent: FileSystemDirectoryHandle
  folderName: string
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

/** Pad a number to two digits. */
function pad2(n: number): string {
  return n.toString().padStart(2, '0')
}

/**
 * `agent-share-YYYY-MM-DDTHHMM` for local `date`, optionally with seconds.
 *
 * The shared name stem: the CLI's mount folder
 * (`crates/agent-share/src/mount/consume.rs`), this app's mount folder, and the
 * download-all zip all wear it, so a share's artifacts sort together wherever
 * they land. Local time, like the CLI's `chrono::Local`.
 */
export function shareStamp(date: Date = new Date(), seconds = false): string {
  const y = date.getFullYear()
  const mo = pad2(date.getMonth() + 1)
  const d = pad2(date.getDate())
  const h = pad2(date.getHours())
  const mi = pad2(date.getMinutes())
  const s = seconds ? pad2(date.getSeconds()) : ''
  return `agent-share-${y}-${mo}-${d}T${h}${mi}${s}`
}

/**
 * Folder-name candidates for local `date`, in collision-retry order.
 * Matches the CLI: `agent-share-YYYY-MM-DDTHHMM`, then with seconds, then `-N`.
 */
export function mountFolderCandidates(date: Date = new Date()): string[] {
  const minute = shareStamp(date)
  const second = shareStamp(date, true)
  const names = [minute, second]
  for (let n = 2; n <= 99; n++) names.push(`${second}-${n}`)
  return names
}

/** True when `parent` already has an entry named `name`. */
async function hasEntry(parent: FileSystemDirectoryHandle, name: string): Promise<boolean> {
  try {
    await parent.getDirectoryHandle(name)
    return true
  } catch (error) {
    if (error instanceof DOMException && error.name === 'NotFoundError') {
      try {
        await parent.getFileHandle(name)
        return true
      } catch (inner) {
        if (inner instanceof DOMException && inner.name === 'NotFoundError') return false
        throw inner
      }
    }
    throw error
  }
}

/** Create `agent-share-…/` under `parent`; retry on name collision. */
async function createMountFolder(
  parent: FileSystemDirectoryHandle,
): Promise<{ root: FileSystemDirectoryHandle; folderName: string }> {
  for (const folderName of mountFolderCandidates()) {
    if (await hasEntry(parent, folderName)) continue
    const root = await parent.getDirectoryHandle(folderName, { create: true })
    return { root, folderName }
  }
  throw new MountError('Could not create a unique agent-share folder')
}

/**
 * Ask the user for a writable parent directory and create `agent-share-…/`
 * under it. The parent may already contain other files.
 *
 * AbortError from the picker is rethrown so the caller can treat cancel as a
 * no-op; everything else becomes a MountError.
 */
export async function pickMountRoot(): Promise<MountSession> {
  const picker = window.showDirectoryPicker
  if (!picker) {
    throw new MountError('This browser cannot mount folders')
  }
  const parent = await picker({ mode: 'readwrite' })
  const { root, folderName } = await createMountFolder(parent)
  return { root, parent, folderName }
}

/** Remove the mount folder from its parent. Best-effort. */
export async function disposeMount(session: MountSession): Promise<void> {
  try {
    await session.parent.removeEntry(session.folderName, { recursive: true })
  } catch (error) {
    if (error instanceof DOMException && error.name === 'NotFoundError') return
    console.warn('[share] failed to remove mount folder', error)
  }
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
  signal?: AbortSignal,
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
      throwIfAborted(signal)
      const want = Math.min(CHUNK, file.size - offset)
      const chunk = await reader.read(file.index, BigInt(offset), want)
      if (chunk.length === 0) {
        if (reader.source_is_origin === false) {
          // A seeder that cannot finish must fail the file, not shorten it —
          // its answer is a snapshot claim, and truncation here is exactly
          // the silent corruption guard #2 exists to stop.
          throw new MountError(
            `${file.path}: the seeder stopped short of the size the manifest describes`,
          )
        }
        break
      }
      // Copy: wasm may hand back a view over SharedArrayBuffer-backed memory.
      await writable.write(new Uint8Array(chunk))
      // Mirroring to a local folder seeds too. Same fire-and-forget contract as
      // the download path: never awaited, never fatal.
      void reader.keep?.(file.index, BigInt(offset), chunk).catch((error: unknown) => {
        console.debug('[share] keeping a mirrored chunk failed', error)
      })
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

function throwIfAborted(signal?: AbortSignal): void {
  if (signal?.aborted) {
    throw new DOMException('Mount cancelled', 'AbortError')
  }
}

/**
 * Bring `root` in line with the current tree.
 *
 * Only files that are new or whose size/mtime/index changed are rewritten.
 * Paths that left the manifest are deleted. Progress totals cover bytes of
 * files that will actually be written this pass.
 *
 * `signal` cancels between chunks; a cancelled pass leaves a partial tree and
 * does not return a new synced state — callers should discard or retry.
 */
export async function syncMount(
  root: FileSystemDirectoryHandle,
  reader: Reader,
  files: FileNode[],
  dirs: ManifestDir[],
  previous: SyncedState,
  onProgress?: (progress: Progress) => void,
  signal?: AbortSignal,
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
    throwIfAborted(signal)
    const parts = safeSplit(rel)
    if (!parts) continue
    await ensureDir(root, parts)
  }

  const total = toWrite.reduce((sum, file) => sum + file.size, 0)
  let done = 0
  onProgress?.({ done, total })

  let writeFailures = 0
  for (const file of toWrite) {
    throwIfAborted(signal)
    try {
      await writeFile(
        root,
        reader,
        file,
        (n) => {
          done += n
          onProgress?.({ done, total })
        },
        signal,
      )
    } catch (error) {
      if (error instanceof DOMException && error.name === 'AbortError') throw error
      writeFailures += 1
      console.warn(`[share] failed to sync ${file.path}; will retry`, error)
      // Omit so the next pass treats it as still needing a write.
      nextFiles.delete(file.path)
    }
  }
  if (toWrite.length > 0 && writeFailures === toWrite.length) {
    throw new MountError('Mount sync failed for every file in this batch')
  }

  // Delete files that disappeared, deepest paths first so parents can go next.
  const removedFiles = [...previous.files.keys()].filter((path) => !nextFiles.has(path))
  removedFiles.sort((a, b) => b.length - a.length)
  for (const path of removedFiles) {
    throwIfAborted(signal)
    await removePath(root, path)
  }

  const removedDirs = [...previous.dirs].filter((path) => !nextDirs.has(path))
  removedDirs.sort((a, b) => b.length - a.length)
  for (const path of removedDirs) {
    throwIfAborted(signal)
    await removePath(root, path)
  }

  return { files: nextFiles, dirs: nextDirs }
}
