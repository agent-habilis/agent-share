/**
 * What this tab holds, and can therefore seed.
 *
 * The browser half of "every peer a seeder". `sync` pulls bytes into local
 * storage; this module is how the UI says which ones landed.
 *
 * A file is seedable only when it is held **in full**. Partial holdings exist
 * in the store and are perfectly useful to the protocol, but they are not what
 * this view answers: telling someone a file is available when only part of it
 * is sends them to a peer that cannot finish the job.
 */

import type { DirNode, FileNode, Node } from './tree.ts'
import { filesUnder } from './tree.ts'

/** How much of a node this tab holds. */
export type SeedState = 'none' | 'partial' | 'full'

/**
 * Whether a file is held.
 *
 * `held` is the set of manifest indices the wasm client reports, which is the
 * same address space a `READ` uses and the same one the availability grid
 * paints. Paths are not used here on purpose: two files can be renamed into
 * each other's place, and an index cannot.
 */
export function fileSeedState(node: FileNode, held: ReadonlySet<number>): SeedState {
  return held.has(node.index) ? 'full' : 'none'
}

/**
 * How much of a folder this tab holds.
 *
 * `partial` is its own state rather than being rounded to one of the others.
 * Rounding down hides real progress on a large folder; rounding up claims files
 * that were never fetched. Neither is something the user can act on, and the
 * whole point of the indicator is deciding whether to press Sync.
 */
export function dirSeedState(node: DirNode, held: ReadonlySet<number>): SeedState {
  const files = filesUnder(node)
  if (files.length === 0) return 'none'
  let count = 0
  for (const file of files) if (held.has(file.index)) count += 1
  if (count === 0) return 'none'
  return count === files.length ? 'full' : 'partial'
}

/** How much of any node this tab holds. */
export function seedState(node: Node, held: ReadonlySet<number>): SeedState {
  return node.kind === 'file'
    ? fileSeedState(node, held)
    : dirSeedState(node, held)
}

/** Files under `node` this tab does not hold yet. */
export function missingUnder(node: Node, held: ReadonlySet<number>): FileNode[] {
  const files = node.kind === 'file' ? [node] : filesUnder(node)
  return files.filter((file) => !held.has(file.index))
}

/**
 * Label for a node's seeding state.
 *
 * "seeding" rather than "downloaded", deliberately. Holding the bytes is not
 * the interesting part — other people being able to get them from you is, and
 * that is what actually happens once a file is held.
 */
export function seedLabel(state: SeedState, node: Node, held: ReadonlySet<number>): string {
  if (node.kind === 'file') {
    return state === 'full' ? 'seeding' : 'not held'
  }
  const files = filesUnder(node)
  const count = files.filter((file) => held.has(file.index)).length
  if (state === 'full') return `seeding all ${files.length}`
  if (state === 'none') return 'not held'
  return `seeding ${count} of ${files.length}`
}

/** Progress across the whole share, for the top bar. */
export function shareSeedSummary(
  root: DirNode,
  held: ReadonlySet<number>,
): { held: number; total: number; state: SeedState } {
  const files = filesUnder(root)
  const count = files.filter((file) => held.has(file.index)).length
  const state: SeedState =
    count === 0 ? 'none' : count === files.length ? 'full' : 'partial'
  return { held: count, total: files.length, state }
}
