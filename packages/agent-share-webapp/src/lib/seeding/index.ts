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

import type { DirNode, FileNode, Node } from '../tree.ts'
import { filesUnder } from '../tree.ts'

/** How much of a node this tab holds. */
export type SeedState = 'none' | 'partial' | 'full'

/**
 * How much of each file this tab holds, in `0.0..=1.0`, by manifest index.
 *
 * Absent means "we know nothing about that file", which is not the same as
 * holding none of it — a slot with no chunk row has never been asked about.
 */
export type Coverage = ReadonlyMap<number, number>

/** Nothing known, for callers with no coverage to hand. */
export const NO_COVERAGE: Coverage = new Map()

/**
 * How much of a file this tab holds.
 *
 * `held` is the set of manifest indices the wasm client reports as complete,
 * which is the same address space a `READ` uses and the same one the
 * availability grid paints. Paths are not used here on purpose: two files can
 * be renamed into each other's place, and an index cannot.
 *
 * **A file can now be `partial`, which it never could before.** Chunks are
 * addressed and verified one at a time, so a peer holding part of a file serves
 * that part — which means a cancelled download, an abandoned preview and a
 * transfer still in flight are each worth something to the swarm, and worth
 * showing. Rounding that down to "not held" would tell the user their tab is
 * contributing nothing when it is.
 */
export function fileSeedState(
  node: FileNode,
  held: ReadonlySet<number>,
  coverage: Coverage = NO_COVERAGE,
): SeedState {
  if (held.has(node.index)) return 'full'
  const fraction = coverage.get(node.index) ?? 0
  if (fraction >= 1) return 'full'
  return fraction > 0 ? 'partial' : 'none'
}

/**
 * How much of a folder this tab holds.
 *
 * `partial` is its own state rather than being rounded to one of the others.
 * Rounding down hides real progress on a large folder; rounding up claims files
 * that were never fetched. Neither is something the user can act on, and the
 * whole point of the indicator is deciding whether to press Seed.
 */
export function dirSeedState(
  node: DirNode,
  held: ReadonlySet<number>,
  coverage: Coverage = NO_COVERAGE,
): SeedState {
  const files = filesUnder(node)
  if (files.length === 0) return 'none'
  let complete = 0
  let touched = 0
  for (const file of files) {
    const state = fileSeedState(file, held, coverage)
    if (state === 'full') complete += 1
    if (state !== 'none') touched += 1
  }
  if (touched === 0) return 'none'
  return complete === files.length ? 'full' : 'partial'
}

/** How much of any node this tab holds. */
export function seedState(
  node: Node,
  held: ReadonlySet<number>,
  coverage: Coverage = NO_COVERAGE,
): SeedState {
  return node.kind === 'file'
    ? fileSeedState(node, held, coverage)
    : dirSeedState(node, held, coverage)
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
export function seedLabel(
  state: SeedState,
  node: Node,
  held: ReadonlySet<number>,
  coverage: Coverage = NO_COVERAGE,
): string {
  if (node.kind === 'file') {
    if (state === 'full') return 'seeding'
    if (state === 'partial') {
      const percent = Math.round((coverage.get(node.index) ?? 0) * 100)
      // Floored at 1 %, because a file a peer can genuinely serve part of must
      // never read as "seeding 0%" — that is the one number that would make a
      // useful source look like a useless one.
      return `seeding ${Math.max(percent, 1)}%`
    }
    return 'not held'
  }
  const files = filesUnder(node)
  const count = files.filter((file) => held.has(file.index)).length
  if (state === 'full') return `seeding all ${files.length}`
  if (state === 'none') return 'not held'
  return `seeding ${count} of ${files.length}`
}
