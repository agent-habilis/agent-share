import { msCss as css, msStyle } from '../../styles/css.ts'
import { oneRow } from '../../styles/mixins.ts'
import { roleVar, T } from '../../tokens.ts'
import { glyphs } from '../../theme/glyphs.ts'
import type { ColorRole } from '../../theme/semantic.ts'

export interface ProgressBarProps {
  value: number
  width?: number
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
})

const TRACK = css({
  flex: 'none',
  color: T.msFgSubtle,
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

export function ProgressBar({
  value,
  width = 24,
  showValue = true,
  tone = 'accent',
  label,
  class: className,
  ...rest
}: ProgressBarProps) {
  const clamped = Math.min(1, Math.max(0, value))
  const filled = Math.round(clamped * width)
  const percent = Math.round(clamped * 100)

  return (
    <span
      role="progressbar"
      aria-label={label}
      aria-valuenow={percent}
      aria-valuemin={0}
      aria-valuemax={100}
      class={className}
      {...rest}
    >
      {msStyle(WRAPPER)}
      <span aria-hidden="true">
        {msStyle(TRACK)}
        <span style={{ color: roleVar[tone] }}>
          {msStyle(FILL)}
          {glyphs.bar.fill.repeat(filled)}
        </span>
        {glyphs.bar.empty.repeat(width - filled)}
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
