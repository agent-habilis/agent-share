/**
 * The bento and its panels — the layout `/info` and `/lab` share.
 *
 * Both pages answer a set of separate questions and neither is a document, so
 * both lay their answers out as a grid of filled boxes rather than as one
 * column of labelled lines. Extracted from `TechInfo` so the two cannot drift:
 * a panel that reads one way on `/info` and another on `/lab` would be two
 * designs wearing the same name.
 */

import { Box, Text, rows } from 'moonspace-dom'
import type { Child } from 'visage-dom/types'
import { Style, css, raw } from 'visage-style'

/** Where the bento goes from one column to two. */
export const WIDE_PX = 960

/**
 * The bento.
 *
 * `minmax(0, 1fr)` rather than `1fr`, because a track's automatic minimum is its
 * content — a table would refuse to narrow and push the column beside it off the
 * page. Each `Box` snaps its own width down to a whole cell, so a fractional
 * track costs nothing.
 *
 * The max width is two measures: the design system's 80ch is what a line of
 * prose wants, and a panel is a line of prose plus its padding. Wider than that
 * and the eye loses the start of the next line.
 */
const BENTO = css({
  display: 'grid',
  gridTemplateColumns: raw('minmax(0, 1fr)'),
  rowGap: raw('var(--ms-row)'),
  columnGap: '2ch',
  maxWidth: '164ch',
  marginInline: 'auto',
  [`@media (min-width: ${WIDE_PX}px)`]: {
    gridTemplateColumns: raw('repeat(2, minmax(0, 1fr))'),
  },
})

/**
 * Applied to a panel that wants the whole row once there are two columns.
 *
 * A stylesheet rendered *inside* the panel rather than a selector in `BENTO`,
 * because `Box` owns its own `dataset` and would overwrite anything passed
 * through for a parent selector to hook. `Style()` scopes to its parent
 * element, so this lands on the Box itself.
 */
const WIDE = css({
  [`@media (min-width: ${WIDE_PX}px)`]: { gridColumn: raw('1 / -1') },
})

/** The grid the panels sit in. */
export function Bento(props: { children?: Child }) {
  return (
    <div>
      {Style(BENTO)}
      {props.children}
    </div>
  )
}

/**
 * One panel of the bento.
 *
 * `Box`'s own `title` is not used, and the reason is the rule it draws under the
 * label. That rule earns its place on a bordered box, where it continues the
 * frame; on a filled one it is a second divider inside a shape that has already
 * divided itself, and six of them read as a page full of lines. The row of space
 * it occupied stays — the label still needs air under it, just not ink.
 */
export function Panel(props: { title: string; wide?: boolean; children?: Child }) {
  return (
    <Box background="bgRaised" padX={2} padY={1}>
      {props.wide ? Style(WIDE) : null}
      <Text as="div" color="fgMuted" caps>
        {props.title}
      </Text>
      <div style={{ height: rows(1) }} />
      {props.children}
    </Box>
  )
}
