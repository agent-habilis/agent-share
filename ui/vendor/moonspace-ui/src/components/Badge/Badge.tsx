import type { Child } from 'visage-dom'
import { raw } from 'visage-style'
import { msCss as css, msStyle } from '../../styles/css.ts'
import { oneRow } from '../../styles/mixins.ts'
import { T } from '../../tokens.ts'

export type BadgeTone = 'neutral' | 'accent' | 'success' | 'warning' | 'danger' | 'info'

export interface BadgeProps {
  tone?: BadgeTone
  variant?: 'solid' | 'outline'
  children?: Child
  class?: string
  id?: string
}

const BADGE = css({
  ...oneRow,
  display: 'inline-flex',
  alignItems: 'center',
  whiteSpace: 'nowrap',
  textTransform: 'uppercase',
  '&[data-variant="solid"]': {
    padding: raw('0 1ch'),
    background: T.msAccent,
    color: T.msBg,
  },
  '&[data-variant="solid"][data-tone="neutral"]': {
    background: T.msFgMuted,
    color: T.msBg,
  },
  '&[data-variant="solid"][data-tone="accent"]': {
    background: T.msAccent,
    color: T.msBg,
  },
  '&[data-variant="solid"][data-tone="success"]': {
    background: T.msSuccess,
    color: T.msBg,
  },
  '&[data-variant="solid"][data-tone="warning"]': {
    background: T.msWarning,
    color: T.msBg,
  },
  '&[data-variant="solid"][data-tone="danger"]': {
    background: T.msDanger,
    color: T.msBg,
  },
  '&[data-variant="solid"][data-tone="info"]': {
    background: T.msInfo,
    color: T.msBg,
  },
  '&[data-variant="outline"]': {
    padding: 0,
    '&::before': { content: raw("'‹'") },
    '&::after': { content: raw("'›'") },
  },
  '&[data-variant="outline"][data-tone="neutral"]': { color: T.msFgMuted },
  '&[data-variant="outline"][data-tone="accent"]': { color: T.msAccent },
  '&[data-variant="outline"][data-tone="success"]': { color: T.msSuccess },
  '&[data-variant="outline"][data-tone="warning"]': { color: T.msWarning },
  '&[data-variant="outline"][data-tone="danger"]': { color: T.msDanger },
  '&[data-variant="outline"][data-tone="info"]': { color: T.msInfo },
})

export function Badge({
  tone = 'neutral',
  variant = 'solid',
  children,
  class: className,
  ...rest
}: BadgeProps) {
  return (
    <span
      {...rest}
      {...(className !== undefined ? { class: className } : {})}
      data-tone={tone}
      data-variant={variant}
    >
      {msStyle(BADGE)}
      {children}
    </span>
  )
}
