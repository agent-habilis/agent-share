/**
 * How much of the share this tab actually holds, weighted by bytes.
 *
 * The peer rows count *slots* — one square per file — because that is what a
 * peer can honestly answer for about someone else. For our own holdings we know
 * the sizes, so the headline number is by byte instead: "3 of 8 files" reads as
 * good progress on a share that is one 4 GB video and seven README files, and
 * it is not.
 */

import { fileSeedState, type Coverage } from '../../../lib/seeding/index.ts'
import type { FileNode } from '../../../lib/tree.ts'

export interface ShareProgress {
  bytesHeld: number
  bytesTotal: number
  /** `0..=1`, and 0 rather than NaN for an empty share. */
  fraction: number
  /** Files held in full — the ones this tab can serve end to end. */
  filesComplete: number
}

export function shareProgress(
  files: readonly FileNode[],
  held: ReadonlySet<number>,
  coverage: Coverage,
): ShareProgress {
  let bytesHeld = 0
  let bytesTotal = 0
  let filesComplete = 0
  for (const file of files) {
    bytesTotal += file.size
    const state = fileSeedState(file, held, coverage)
    if (state === 'full') {
      bytesHeld += file.size
      filesComplete += 1
      continue
    }
    if (state === 'none') continue
    // A partial slot's fraction is of that file, not of the share — and it is
    // clamped because a store that over-reports would otherwise push the whole
    // bar past 100%.
    const fraction = Math.min(1, Math.max(0, coverage.get(file.index) ?? 0))
    bytesHeld += file.size * fraction
  }
  return {
    bytesHeld,
    bytesTotal,
    fraction: bytesTotal > 0 ? bytesHeld / bytesTotal : 0,
    filesComplete,
  }
}
