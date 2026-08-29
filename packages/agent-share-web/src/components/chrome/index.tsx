import { Stack, Text, t } from 'moonspace-dom'
import type { Child } from 'visage-dom'
import { Style, css, raw } from 'visage-style'

import { Brand } from '../brand/index.tsx'
import { NARROW } from '../../lib/breakpoints.ts'

// The name never breaks at its hyphen, and nothing else in the row may
// overlap it: it is the one thing the row is built around.
const NAME = css({
  whiteSpace: 'nowrap',
  flexShrink: raw('0'),
})

/*
  Fixed and opaque at the top edge: that is also what iOS 26 Safari samples to
  tint its status bar (see app.css). The safe-area term in the padding reaches
  under that status bar (viewport-fit=cover).

  On a phone the slots stack, one row each, so the bar is one to three rows
  tall. It goes `sticky` there — in flow, so the pane under it needs no padding
  sized to a height that varies — and Safari samples a sticky element at the
  top edge exactly as it does a fixed one.
*/
const BAR = css({
  position: 'fixed',
  top: raw('0'),
  left: raw('0'),
  right: raw('0'),
  zIndex: raw('1'),
  padding: raw('calc(var(--ms-row) + env(safe-area-inset-top, 0px)) 2ch var(--ms-row)'),
  display: 'grid',
  gridTemplateColumns: raw('1fr auto'),
  alignItems: 'center',
  gap: '2ch',
  // `minmax(0, auto)` for the middle, not plain `auto`: an `auto` track
  // refuses to shrink below its content, so on a narrow window the actions
  // overflow *into* the status and the two draw on top of each other. This
  // lets the status be the one that gives.
  '&[data-slots="three"]': {
    gridTemplateColumns: raw('minmax(0, 1fr) minmax(0, auto) minmax(0, 1fr)'),
    [NARROW]: { gridTemplateColumns: raw('1fr') },
  },
  [NARROW]: {
    position: 'sticky',
    gridTemplateColumns: raw('1fr'),
    rowGap: raw('0'),
    paddingLeft: '1ch',
    paddingRight: '1ch',
  },
})

const CENTER = css({
  display: 'flex',
  justifyContent: 'center',
  // Clips the status on a window too narrow for everything rather than
  // shoving the actions off the edge.
  overflow: 'hidden',
  [NARROW]: { justifyContent: 'flex-start' },
})

const TRAILING = css({
  display: 'flex',
  justifyContent: 'flex-end',
  minWidth: raw('0'),
  [NARROW]: { justifyContent: 'flex-start' },
})

// The bar is out of flow: one row of content, one row of padding above and
// below, plus the status-bar inset it reaches under. In flow on a phone.
const PANE = css({
  paddingTop: raw('calc(3 * var(--ms-row) + env(safe-area-inset-top, 0px))'),
  [NARROW]: { paddingTop: raw('0') },
})

/**
 * App chrome: top bar on the sunken page background + content surface on `bg`
 * (same split as the file browser).
 */
export function Chrome({
  crumb,
  center,
  trailing,
  toast,
  children,
}: {
  /** When omitted, the top bar is just the brand. */
  crumb?: string
  /** Status between the brand and the actions. Must be exactly `oneRow` tall. */
  center?: Child
  trailing?: Child
  /**
   * Something gone wrong, given the whole bar but the brand. Must be exactly
   * `oneRow` tall, like everything else in that row.
   */
  toast?: Child
  children: Child
}) {
  return (
    <div
      style={{
        display: 'flex',
        flexDirection: 'column',
        // The large viewport plus the home-indicator inset, which iOS keeps out
        // of lvh even with viewport-fit=cover: the page paints to the screen
        // edge and scrolling panes pad their end by --bottom-inset (app.css).
        height: 'calc(100lvh + env(safe-area-inset-bottom, 0px))',
        minHeight: 0,
        paddingLeft: 'env(safe-area-inset-left, 0px)',
        paddingRight: 'env(safe-area-inset-right, 0px)',
      }}
    >
      {/*
        One row, and only ever one row — one per slot on a phone, where they
        stack. Every child of this bar is exactly `oneRow` tall — that is what
        lets the content below it stay put while a transfer swaps the actions
        for a progress bar, or a failure swaps the whole bar for a toast.

        A grid, not a flex row, and only because of the middle slot. Centring
        the status in the *slack* between the two ends ties its position to
        their widths, and both ends change width on their own schedule — the
        trailing end swaps its whole button row for a progress bar and `Cancel`
        mid-transfer, and relabels `Seed` / `Seeding` and `Mount` / `Unmount`
        besides. Measured, that dragged the whole readout 23px sideways
        mid-transfer, which is precisely the jitter the readout's own
        fixed-width fields exist to prevent, arriving one level up.

        Equal `minmax(0, 1fr)` rails on either side of an `auto` middle pin the
        middle to the *container's* centre instead, so it holds still whatever
        the ends do. `minmax(0, …)` rather than `1fr` so a rail may shrink below
        its content on a narrow window; the middle is the one thing that must
        not move.

        A toast takes the same three rails, which is why it is a bare run of
        cells rather than a component with a grid of its own — its message lands
        exactly where the status it replaced was drawn.
      */}
      <div
        data-slots={toast || center ? 'three' : 'two'}
        style={{ flexShrink: 0, background: t.bgSunken }}
      >
        {/*
          A toast takes this row rather than being given one under it. A second
          line appearing would push every pixel of the page down and pull it
          back up again seconds later. It takes all of it, brand included:
          crumb, status and actions can all wait a few seconds, and the name of
          the app is not news to anyone reading a failure.
        */}
        {Style(BAR)}
        {toast ?? (
          <>
            <Stack direction="row" gap={1}>
              {Style(NAME)}
              {/*
                The name doubles as the agent indicator — it goes green once a
                tool has been called. Carried by the brand rather than by
                something beside it because an element that appears would move
                the crumb sideways, and because what it reports is about the
                whole page rather than about what you can do next.
              */}
              <Brand />
              {crumb ? (
                <>
                  <Text color="fgMuted">/</Text>
                  <Text color="fgMuted">{crumb}</Text>
                </>
              ) : null}
            </Stack>
            {center ? (
              <div>
                {Style(CENTER)}
                {center}
              </div>
            ) : null}
            <div>
              {Style(TRAILING)}
              {trailing ?? null}
            </div>
          </>
        )}
      </div>
      <div
        style={{
          flex: 1,
          minHeight: 0,
          display: 'flex',
          flexDirection: 'column',
          background: t.bg,
        }}
      >
        {Style(PANE)}
        {children}
      </div>
      <div class="edge-tint-bottom" />
    </div>
  )
}
