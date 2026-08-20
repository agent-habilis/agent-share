import { Stack, Text, t } from 'moonspace-dom'
import type { Child } from 'visage-dom'

import { Brand } from '../brand/index.tsx'

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
        height: '100vh',
        minHeight: 0,
      }}
    >
      {/*
        One row, and only ever one row. Every child of this bar is exactly
        `oneRow` tall — that is what lets the content below it stay put while a
        transfer swaps the actions for a progress bar, or a failure swaps the
        whole bar for a toast.

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
        style={{
          flexShrink: 0,
          padding: 'var(--ms-row) 2ch',
          display: 'grid',
          // `minmax(0, auto)` for the middle, not plain `auto`: an `auto`
          // track refuses to shrink below its content, so on a narrow window
          // the actions overflow *into* the status and the two draw on top of
          // each other. This lets the status be the one that gives.
          gridTemplateColumns:
            toast || center ? 'minmax(0, 1fr) minmax(0, auto) minmax(0, 1fr)' : '1fr auto',
          alignItems: 'center',
          gap: '2ch',
        }}
      >
        {/*
          A toast takes this row rather than being given one under it. A second
          line appearing would push every pixel of the page down and pull it
          back up again seconds later. It takes all of it, brand included:
          crumb, status and actions can all wait a few seconds, and the name of
          the app is not news to anyone reading a failure.
        */}
        {toast ?? (
          <>
            <Stack direction="row" gap={1}>
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
              // `overflow: hidden` so a window too narrow for everything clips
              // the status rather than shoving the actions off the edge.
              <div style={{ display: 'flex', justifyContent: 'center', overflow: 'hidden' }}>
                {center}
              </div>
            ) : null}
            <div style={{ display: 'flex', justifyContent: 'flex-end', minWidth: 0 }}>
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
        {children}
      </div>
    </div>
  )
}
