import { component, signal } from 'visage-dom'
import { msCss as css, msStyle } from '../../styles/css.ts'
import { oneRow } from '../../styles/mixins.ts'
import { measureCells } from '../../hooks/measureCells.ts'
import { roleVar, T } from '../../tokens.ts'
import { glyphs } from '../../theme/glyphs.ts'
import type { ColorRole } from '../../theme/semantic.ts'

export interface ProgressBarProps {
  value: number
  width?: number
  /** Fill the container instead of drawing a fixed `width` of cells. */
  fluid?: boolean
  showValue?: boolean
  tone?: Extract<ColorRole, 'accent' | 'success' | 'warning' | 'danger'>
  label: string
  class?: string
  id?: string
}

const WRAPPER = css({
  ...oneRow,
  display: 'inline-flex',
  alignItems: 'center',
  gap: '1ch',
  whiteSpace: 'pre',
  '&[data-fluid="true"]': {
    display: 'flex',
    flex: '1',
    minWidth: 0,
  },
})

const TRACK = css({
  flex: 'none',
  color: T.msFgSubtle,
  // Zero basis, so the measured width comes from the container rather than
  // from the glyphs already in it — otherwise every measurement would feed
  // the next one.
  '&[data-fluid="true"]': {
    flex: '1',
    minWidth: 0,
    overflow: 'hidden',
  },
})

const FILL = css({
  color: roleVar.accent,
})

const VALUE = css({
  flex: 'none',
  width: '4ch',
  textAlign: 'right',
  color: T.msFgMuted,
})

export const ProgressBar = component<ProgressBarProps>(function* (props) {
  /** Track width in cells, once the element has been laid out. Fluid only. */
  const measured = signal<number | null>(null)

  yield () => {
    const {
      value,
      width = 24,
      fluid = false,
      showValue = true,
      tone = 'accent',
      label,
      class: className,
      id,
    } = props

    const clamped = Math.min(1, Math.max(0, value))
    const percent = Math.round(clamped * 100)
    // Before the first measurement lands there is nothing to draw — the
    // ResizeObserver fires on the very next frame.
    const cells = fluid ? (measured.value ?? 0) : width
    const filled = Math.round(clamped * cells)

    return (
      <span
        role="progressbar"
        aria-label={label}
        aria-valuenow={percent}
        aria-valuemin={0}
        aria-valuemax={100}
        class={className}
        id={id}
        data-fluid={fluid ? 'true' : 'false'}
      >
        {msStyle(WRAPPER)}
        <span
          aria-hidden="true"
          data-fluid={fluid ? 'true' : 'false'}
          ref={
            fluid
              ? (el) => {
                  const node = el as HTMLElement
                  const update = () => {
                    measured.value = measureCells(node)
                  }
                  update()
                  const ro = new ResizeObserver(update)
                  ro.observe(node)
                  if ('fonts' in document) {
                    void document.fonts.ready.then(update)
                  }
                  return () => ro.disconnect()
                }
              : undefined
          }
        >
          {msStyle(TRACK)}
          <span style={{ color: roleVar[tone] }}>
            {msStyle(FILL)}
            {glyphs.bar.fill.repeat(filled)}
          </span>
          {glyphs.bar.empty.repeat(cells - filled)}
        </span>
        {showValue && (
          <span aria-hidden="true">
            {msStyle(VALUE)}
            {percent}%
          </span>
        )}
      </span>
    )
  }
})
