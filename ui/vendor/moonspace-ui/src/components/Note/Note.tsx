import type { Child } from 'visage-dom'
import { Style, css, raw } from 'visage-style'
import { roleVar } from '../../tokens.ts'
import { T } from '../../tokens.ts'
import type { ColorRole } from '../../theme/semantic.ts'

export type NoteTone = 'info' | 'success' | 'warning' | 'danger'

export interface NoteProps {
  tone?: NoteTone
  label?: string
  children?: Child
  class?: string
  id?: string
}

const toneStyle: Record<NoteTone, { color: ColorRole; marker: string; label: string }> = {
  info: { color: 'info', marker: 'i', label: 'note' },
  success: { color: 'success', marker: '✓', label: 'ok' },
  warning: { color: 'warning', marker: '!', label: 'warn' },
  danger: { color: 'danger', marker: '✗', label: 'error' },
}

const WRAPPER = css({
  display: 'flex',
  gap: '1ch',
  padding: raw('0 1ch'),
  background: T.msBgSunken,
} as never)

const MARKER = css({
  flex: 'none',
  width: '1ch',
  height: 'var(--ms-row)',
  lineHeight: 'var(--ms-row)',
} as never)

const LABEL = css({
  flex: 'none',
  height: 'var(--ms-row)',
  lineHeight: 'var(--ms-row)',
  textTransform: 'uppercase',
} as never)

const BODY = css({
  minWidth: 0,
} as never)

export function Note({ tone = 'info', label, children, class: className, ...rest }: NoteProps) {
  const style = toneStyle[tone]
  const accent = roleVar[style.color]

  return (
    <div
      {...rest}
      {...(className !== undefined ? { class: className } : {})}
      style={{ boxShadow: `inset 1px 0 0 0 ${accent}` }}
    >
      {Style(WRAPPER)}
      <span aria-hidden="true" style={{ color: accent }}>
        {Style(MARKER)}
        {style.marker}
      </span>
      <span style={{ color: accent }}>
        {Style(LABEL)}
        {label ?? style.label}
      </span>
      <div>
        {Style(BODY)}
        {children}
      </div>
    </div>
  )
}
