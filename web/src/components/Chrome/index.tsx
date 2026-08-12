import { Stack, Text, t } from 'moonspace-dom'
import type { Child } from 'visage-dom'

import { AgentBadge } from '../AgentBadge/index.tsx'

/**
 * App chrome: top bar on the sunken page background + content surface on `bg`
 * (same split as the file browser). `belowBar` is optional status under the
 * main top-bar line.
 */
export function Chrome({
  crumb,
  center,
  trailing,
  belowBar,
  children,
}: {
  /** When omitted, the top bar is just the brand. */
  crumb?: string
  /** Status between the brand and the actions. Must be exactly `oneRow` tall. */
  center?: Child
  trailing?: Child
  belowBar?: Child
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
      <div
        style={{
          flexShrink: 0,
          padding: 'var(--ms-row) 2ch',
          display: 'flex',
          flexDirection: 'column',
          gap: 'calc(1 * var(--ms-row))',
        }}
      >
        {/*
          A grid, not a flex row, and only because of the middle slot.

          Centring the status in the *slack* between the two ends ties its
          position to their widths, and both ends change width on their own
          schedule — the trailing end swaps its whole button row for a progress
          bar and `Cancel` mid-transfer, and relabels `Seed` / `Seeding` and
          `Mount` / `Unmount` besides. Measured, that dragged the whole readout
          23px sideways mid-transfer, which is precisely the jitter the
          readout's own fixed-width fields exist to prevent, arriving one level
          up.

          Equal `minmax(0, 1fr)` rails on either side of an `auto` middle pin
          the middle to the *container's* centre instead, so it holds still
          whatever the ends do. `minmax(0, …)` rather than `1fr` so a rail may
          shrink below its content on a narrow window; the middle is the one
          thing that must not move.
        */}
        <div
          style={{
            display: 'grid',
            // `minmax(0, auto)` for the middle, not plain `auto`: an `auto`
            // track refuses to shrink below its content, so on a narrow window
            // the actions overflow *into* the status and the two draw on top of
            // each other. This lets the status be the one that gives.
            gridTemplateColumns: center
              ? 'minmax(0, 1fr) minmax(0, auto) minmax(0, 1fr)'
              : '1fr auto',
            alignItems: 'center',
            gap: '2ch',
          }}
        >
          <Stack direction="row" gap={1}>
            <Text weight="bold">agent-share</Text>
            {crumb ? (
              <>
                <Text color="fgMuted">/</Text>
                <Text color="fgMuted">{crumb}</Text>
              </>
            ) : null}
            {/*
              Beside the brand rather than out with the actions: it says
              something about the whole page, not about what you can do next,
              and it must not move the action row when it appears. It renders
              nothing until an agent has actually called a tool.
            */}
            <AgentBadge />
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
        </div>
        {belowBar ?? null}
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
