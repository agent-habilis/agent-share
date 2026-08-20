/**
 * A share of any size, drawn on a grid of fixed size.
 *
 * The per-peer rows can afford one square per slot because a peer's row is one
 * line tall and a share of a hundred files makes a hundred squares. A share of
 * ten thousand does not: the grid would be taller than the pane, and the panel's
 * height would depend on the share, which makes the whole page reflow as a
 * manifest grows.
 *
 * So the grid is a constant, and the *share* is resampled onto it. That is the
 * same trade a BitTorrent client makes when it draws a 400-piece torrent and a
 * 40,000-piece one in the same box, and it is worth stating plainly: above
 * `CELLS` slots, one cell is many files and cannot be pointed at.
 */

import type { SeedState } from '../../../lib/seeding/index.ts'

/**
 * The square's side, in cells.
 *
 * Sixteen rather than something larger because every peer gets one of these and
 * they sit side by side: the square has to be small enough that a handful fit a
 * row, and large enough that a run of missing slots is a visible notch.
 */
export const GRID_SIDE = 16

/** Cells in the whole square. */
export const CELLS = GRID_SIDE * GRID_SIDE

/**
 * Cells in one peer's line.
 *
 * A peer's row in the table is exactly one row tall, so its availability is a
 * line rather than a square — and a fixed one, so twenty peers on twenty
 * different shares all draw the same width and the column never reflows.
 */
export const PEER_CELLS = 20

/**
 * Resample `total` slots onto `cells` cells.
 *
 * Both directions are real. **Downsampling** (more slots than cells) folds a
 * run of slots into one cell, and the fold is deliberately pessimistic: a cell
 * is `full` only when every slot under it is, so a square that reads as complete
 * genuinely is. Over-reporting is the dangerous direction here — this grid is
 * how someone decides whether a share is still recoverable.
 *
 * **Upsampling** (fewer slots than cells) repeats each slot across a block of
 * cells, so an 8-file share fills the square rather than drawing eight specks in
 * the corner.
 */
export function resampleSlots(
  total: number,
  cells: number,
  stateOf: (slot: number) => SeedState,
): SeedState[] {
  if (cells <= 0 || total <= 0) return []
  return Array.from({ length: cells }, (_, cell) => {
    const start = Math.floor((cell * total) / cells)
    // `end` can equal `start` when upsampling — the slot at `start` is the one
    // this cell stands for, and the loop below still runs once for it.
    const end = Math.max(start + 1, Math.floor(((cell + 1) * total) / cells))
    let anyHeld = false
    let allFull = true
    for (let slot = start; slot < end && slot < total; slot += 1) {
      const state = stateOf(slot)
      if (state !== 'none') anyHeld = true
      if (state !== 'full') allFull = false
    }
    if (allFull) return 'full'
    return anyHeld ? 'partial' : 'none'
  })
}

/** Whether one cell stands for more than one slot, so the caller can say so. */
export function isCoarse(total: number, cells: number): boolean {
  return total > cells
}
