/**
 * The character grid.
 *
 * Every dimension in the system is an integer number of cells. One cell is `1ch`
 * wide and one row tall. Nothing is sized in arbitrary pixels — that is the whole
 * contract, and it's what makes a browser render map 1:1 onto terminal columns
 * and rows.
 *
 * `rowPx / fontSizePx` is 1.2. That ratio is not arbitrary: box-drawing glyphs
 * stop connecting vertically much above 120% line-height in most monospace fonts,
 * which would leave visible gaps in every border we draw.
 */
export const grid = {
  /** The one and only font size. There is no type scale. */
  fontSizePx: 15,
  /** Row height in pixels. 15 × 1.2 = 18, an integer — avoids sub-pixel drift when stacked. */
  rowPx: 18,
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
