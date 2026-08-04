import { raw } from 'visage-style'
import { T } from '../tokens.ts'

/**
 * Grid math. Every dimension in the system goes through one of these.
 */

/** `n` cells of horizontal space. */
export const cells = (n: number): string => `${n}ch`

/** `n` rows of vertical space. */
export const rows = (n: number): string => `calc(${n} * var(--ms-row))`

/** Snap a percentage width down to a whole number of cells. */
export const snapWidth = (max?: string) => ({
  width: '100%' as const,
  ...(max ? { maxWidth: raw(max as never) } : {}),
  '@supports (width: round(down, 100%, 1ch))': {
    width: raw('round(down, 100%, 1ch)'),
    ...(max ? { maxWidth: raw(`min(${max}, round(down, 100%, 1ch))` as never) } : {}),
  },
})

/** Inverse video — swap foreground and background. */
export const inverse = {
  background: T.msFg,
  color: T.msBg,
}

/** Focus ring flush against the cell boundary. */
export const focusRing = {
  outline: raw('1px solid var(--ms-accent)'),
  outlineOffset: 0,
}

/**
 * Applied to disabled controls.
 *
 * It deliberately says nothing about the edge. `oneRowChrome` declares its
 * outline `transparent` so the focus ring has something to recolour, which means
 * setting `outline-color` here would not tint an existing edge — it would *draw*
 * one, on the single control that is meant to look most inert.
 */
export const disabled = {
  color: T.msFgSubtle,
  cursor: 'not-allowed' as const,
  pointerEvents: 'none' as const,
}

/** Visually hidden but still announced by screen readers. */
export const srOnly = {
  position: 'absolute' as const,
  width: '1px',
  height: '1px',
  padding: 0,
  margin: '-1px',
  overflow: 'hidden' as const,
  clipPath: raw('inset(50%)'),
  whiteSpace: 'nowrap' as const,
  border: 0,
}

/** A single-row control on the baseline grid. */
export const oneRow = {
  height: raw(rows(1) as never),
  lineHeight: raw(rows(1) as never),
}

/**
 * A single-row control with chrome — padding, an edge, and a box to fill.
 *
 * `1ch` is one cell, so it is the horizontal padding. There is no vertical
 * equivalent: a cell is `1ch` wide but a *row* tall, and `1ch` of vertical
 * padding would leave the control 40.5px — not an integer number of rows. The
 * row's own leading is the vertical padding instead, which is what keeps every
 * control interchangeable with every other one-row thing on the grid.
 *
 * The edge is an `outline`, as it is on Input, Select and Box. An outline costs
 * no layout, so the content box stays a full row and the one global line-height
 * needs no exception here — which is the whole point, since a control that wants
 * its own line-height cannot have one anyway: the un-layered `font: inherit` in
 * global.css is a shorthand, it resets `line-height` along with the family, and
 * un-layered author CSS outranks the `components` layer these rules land in.
 * A `border` would also eat 2px of the control's width, taking it off the cell
 * grid, and would need a transparent one on filled variants just to keep them
 * the same size as outlined ones.
 *
 * The offset is negative so the ring paints just *inside* the border box. An
 * outline drawn outside it is at the mercy of any ancestor that clips, and that
 * is not hypothetical: the file-detail pane's scroller sets only `overflow-y`,
 * which per spec computes `overflow-x` from `visible` to `auto`, so it clips
 * horizontally too — and the download button sits flush against its left edge.
 * The result was a button with three sides. Inset, the ring's outer edge lands
 * on the border box rather than a pixel proud of it, so a control's painted
 * extent is also its layout extent: whole cells by whole rows.
 *
 * Declared transparent up front so variants only ever set `outlineColor`.
 *
 * `border: 0` is not redundant with that: a bare `<button>` gets `2px outset`
 * from the UA stylesheet, which eats 4px of the content box and pushes the
 * control 4px past a whole number of cells. Measured, after the outline landed
 * and this line hadn't yet.
 */
export const oneRowChrome = {
  ...oneRow,
  display: 'inline-flex' as const,
  alignItems: 'center' as const,
  padding: raw('0 1ch'),
  border: 0,
  outline: raw('var(--ms-border-width) solid transparent'),
  outlineOffset: raw('-1px'),
  background: 'transparent',
  cursor: 'pointer' as const,
  whiteSpace: 'nowrap' as const,
}
