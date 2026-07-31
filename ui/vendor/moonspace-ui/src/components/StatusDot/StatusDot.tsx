import type { Child } from 'visage-dom'
import { msCss as css, msStyle } from '../../styles/css.ts'
import { oneRow, srOnly } from '../../styles/mixins.ts'
import { roleVar } from '../../tokens.ts'
import { glyphs } from '../../theme/glyphs.ts'
import type { ColorRole } from '../../theme/semantic.ts'

export type Status = 'ready' | 'building' | 'error' | 'queued' | 'canceled'

export interface StatusDotProps {
  status: Status
  children?: Child
  class?: string
  id?: string
}

const statusStyle: Record<Status, { color: ColorRole; glyph: string }> = {
  ready: { color: 'success', glyph: glyphs.status.ready },
  building: { color: 'warning', glyph: glyphs.status.building },
  error: { color: 'danger', glyph: glyphs.status.error },
  queued: { color: 'fgMuted', glyph: glyphs.status.queued },
  canceled: { color: 'fgSubtle', glyph: glyphs.status.canceled },
}

const WRAPPER = css({
  ...oneRow,
  display: 'inline-flex',
  alignItems: 'center',
  gap: '1ch',
})

const MARKER = css({
  flex: 'none',
  width: '1ch',
})

const HIDDEN = css(srOnly)

export function StatusDot({ status, children, class: className, ...rest }: StatusDotProps) {
  const { color, glyph } = statusStyle[status]

  return (
    <span class={className} {...rest}>
      {msStyle(WRAPPER)}
      <span aria-hidden="true" style={{ color: roleVar[color] }}>
        {msStyle(MARKER)}
        {glyph}
      </span>
      {children != null ? <span>{children}</span> : (
        <span>
          {msStyle(HIDDEN)}
          {status}
        </span>
      )}
    </span>
  )
}
