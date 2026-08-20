/**
 * Describing a share's tree to an agent.
 *
 * Numbers stay numbers here. The UI renders `humanBytes` because a person reads
 * "1.4 GB" faster than the digits; a model does not, and a rounded string is a
 * value it cannot do arithmetic on.
 */

import { nodeAtPath, type DirNode, type FileNode, type Node } from '../tree.ts'
import { ToolInputError } from './result.ts'

export interface Entry {
  kind: 'file' | 'dir'
  path: string
  name: string
  size?: number
  mtime?: number
  /** Directories only: how many entries are directly inside. */
  entries?: number
}

/**
 * The node at `parts`, or a failure naming the path that missed.
 *
 * An empty path is the share root rather than an error — `list` with no
 * arguments is the first call an agent makes, and "list the share" is a
 * reasonable thing for it to mean.
 */
export function locate(root: DirNode, parts: string[]): Node {
  if (parts.length === 0) return root
  const node = nodeAtPath(root, parts)
  if (!node) {
    throw new ToolInputError('not_found', `"${parts.join('/')}" is not in this share`)
  }
  return node
}

export function requireFile(root: DirNode, parts: string[]): FileNode {
  const node = locate(root, parts)
  if (node.kind !== 'file') {
    throw new ToolInputError('not_a_file', `"${parts.join('/')}" is a directory, not a file`)
  }
  return node
}

export function describeEntry(node: Node): Entry {
  if (node.kind === 'file') {
    return { kind: 'file', path: node.path, name: node.name, size: node.size, mtime: node.mtime }
  }
  return { kind: 'dir', path: node.path, name: node.name, entries: node.children.length }
}

/** Flatten `node`'s children into `into`, `depth` levels deep. */
export function collect(node: DirNode, depth: number, into: Entry[]): void {
  for (const child of node.children) {
    into.push(describeEntry(child))
    if (child.kind === 'dir' && depth > 1) collect(child, depth - 1, into)
  }
}
