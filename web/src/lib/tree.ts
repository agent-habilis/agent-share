/**
 * Turn the flat manifest into a navigable tree.
 *
 * The manifest is a remote peer's word for what it holds, so every path is
 * treated as hostile input. This mirrors the hardening the native consumer
 * applies in `src/file/walk.rs` (`safe_component`) and `src/mount/nfs.rs`
 * (`validate_rel_path`): reject absolute paths, any `.` or `..` component, and
 * anything that would escape the root. In a browser a bad path cannot overwrite
 * a file, but it can still forge a tree that misrepresents what is being
 * downloaded — and the same manifest feeds the ZIP writer, where names *do*
 * land on a filesystem.
 */

export interface ManifestDir {
  rel_path: string
  mode: number
  mtime: number
}

export interface ManifestFile {
  rel_path: string
  size: number
  mode: number
  mtime: number
}

export interface Manifest {
  dirs: ManifestDir[]
  files: ManifestFile[]
}

export interface FileNode {
  kind: 'file'
  name: string
  path: string
  /** Index in `manifest.files` — the address READ requests use. */
  index: number
  size: number
  mtime: number
}

export interface DirNode {
  kind: 'dir'
  name: string
  path: string
  children: Node[]
}

export type Node = FileNode | DirNode

/** A path component that is safe to place under a root. */
function safeComponent(component: string): boolean {
  return component !== '' && component !== '.' && component !== '..' && !component.includes('\0')
}

/**
 * Split a manifest path, rejecting anything that escapes the root.
 * Returns `null` for a path that should be dropped rather than rendered.
 */
export function safeSplit(relPath: string): string[] | null {
  if (relPath.startsWith('/') || relPath.includes('\\')) return null
  const parts = relPath.split('/')
  return parts.every(safeComponent) ? parts : null
}

function emptyDir(name: string, path: string): DirNode {
  return { kind: 'dir', name, path, children: [] }
}

/** Find or create the directory chain for `parts`, under `root`. */
function ensureDir(root: DirNode, parts: string[]): DirNode {
  let current = root
  let prefix = ''
  for (const part of parts) {
    prefix = prefix ? `${prefix}/${part}` : part
    let next = current.children.find(
      (child): child is DirNode => child.kind === 'dir' && child.name === part,
    )
    if (!next) {
      next = emptyDir(part, prefix)
      current.children.push(next)
    }
    current = next
  }
  return current
}

/**
 * Build the tree. Paths that fail validation are skipped, and their count is
 * returned so the UI can say so rather than silently showing less than the
 * peer claims to hold.
 */
export function buildTree(manifest: Manifest): { root: DirNode; skipped: number } {
  const root = emptyDir('', '')
  let skipped = 0

  for (const dir of manifest.dirs) {
    const parts = safeSplit(dir.rel_path)
    if (!parts) {
      skipped += 1
      continue
    }
    ensureDir(root, parts)
  }

  manifest.files.forEach((file, index) => {
    // A tombstone: the file at this index is gone, but the slot stays so
    // every later index keeps addressing the file it always did. Not a
    // hostile path, so it must not be counted as one.
    if (file.rel_path === '') return
    const parts = safeSplit(file.rel_path)
    if (!parts || parts.length === 0) {
      skipped += 1
      return
    }
    const name = parts[parts.length - 1] as string
    const parent = ensureDir(root, parts.slice(0, -1))
    parent.children.push({
      kind: 'file',
      name,
      path: file.rel_path,
      index,
      size: file.size,
      mtime: file.mtime,
    })
  })

  sortRecursive(root)
  return { root, skipped }
}

/** Directories first, then files, each alphabetically — Finder's order. */
function sortRecursive(dir: DirNode): void {
  dir.children.sort((left, right) => {
    if (left.kind !== right.kind) return left.kind === 'dir' ? -1 : 1
    return left.name.localeCompare(right.name)
  })
  for (const child of dir.children) {
    if (child.kind === 'dir') sortRecursive(child)
  }
}

/** Every file under `node`, depth-first — what the download button walks. */
export function filesUnder(node: Node): FileNode[] {
  if (node.kind === 'file') return [node]
  return node.children.flatMap(filesUnder)
}

/** The file or directory at `path`, or `undefined` when the path is empty/stale. */
export function nodeAtPath(root: DirNode, path: string[]): Node | undefined {
  if (path.length === 0) return undefined
  let current: Node = root
  for (const name of path) {
    if (current.kind !== 'dir') return undefined
    const next: Node | undefined = current.children.find((child) => child.name === name)
    if (!next) return undefined
    current = next
  }
  return current
}

const UNITS = ['B', 'KB', 'MB', 'GB', 'TB'] as const

export function humanBytes(bytes: number): string {
  let value = bytes
  let unit = 0
  while (value >= 1024 && unit < UNITS.length - 1) {
    value /= 1024
    unit += 1
  }
  return unit === 0 ? `${value} ${UNITS[0]}` : `${value.toFixed(1)} ${UNITS[unit]}`
}
