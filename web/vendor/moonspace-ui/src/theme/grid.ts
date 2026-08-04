/**
 * The character grid.
 *
 * Every dimension in the system is an integer number of cells. One cell is `1ch`
 * wide and one row tall. Nothing is sized in arbitrary pixels — that is the whole
 * contract, and it's what makes a browser render map 1:1 onto terminal columns
 * and rows.
 *
 * Upstream moonspace keeps `rowPx / fontSizePx` at 1.2 for box-drawing joins.
 * This vendor copy runs 25% taller (1.5) for the share browser's readability.
 */

const FONT_SIZE_PX = 15

/**
 * Row height as a multiple of the font size — and, because `--ms-row` is also
 * the one global line-height (`body` in global.css), the leading with it. The
 * ratio is the input; the row is what falls out of it, which is why `rowPx` is
 * computed here rather than written down and kept in sync by hand.
 */
const LINE_HEIGHT = 1.5

export const grid = {
  /** The one and only font size. There is no type scale. */
  fontSizePx: FONT_SIZE_PX,
  lineHeight: LINE_HEIGHT,
  /**
   * The vertical cell, in pixels — 22.5, a quarter taller than upstream's 18px
   * row. Every height, padding and gap in the system is a multiple of it, and so
   * is every line of text, so nothing can drift out from under anything else.
   */
  rowPx: FONT_SIZE_PX * LINE_HEIGHT,
  /** Default content measure. 80 columns, the classic terminal width. */
  measureCh: 80,
  /** Reference terminal viewport, used by the TerminalFrame story decorator. */
  terminal: { cols: 80, rows: 24 },
  /** Border thickness. 1px keeps a border on the cell boundary rather than inside it. */
  borderPx: 1,
} as const

export type Grid = typeof grid

/** CSS custom property names, so the values are reachable from raw CSS and devtools. */
export const gridVars = {
  fontSize: '--ms-font-size',
  row: '--ms-row',
  measure: '--ms-measure',
  border: '--ms-border-width',
} as const
