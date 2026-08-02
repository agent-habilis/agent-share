import type { Child } from 'visage-dom'
import { raw } from 'visage-style'
import { msCss as css, msStyle } from '../../styles/css.ts'
import { focusRing, oneRow } from '../../styles/mixins.ts'
import { T } from '../../tokens.ts'

export type ButtonVariant = 'primary' | 'secondary' | 'ghost' | 'danger'

export interface ButtonProps {
  variant?: ButtonVariant
  block?: boolean
  type?: 'button' | 'submit' | 'reset'
  disabled?: boolean
  children?: Child
  class?: string
  id?: string
  onclick?: (event: MouseEvent) => void
  'aria-label'?: string
}

const BUTTON = css({
  ...oneRow,
  display: 'inline-flex',
  alignItems: 'center',
  padding: 0,
  border: 0,
  background: 'transparent',
  cursor: 'pointer',
  whiteSpace: 'nowrap',
  '&::before': { content: raw("'[ '") },
  '&::after': { content: raw("' ]'") },
  '&[data-variant="primary"]': {
    background: T.msAccent,
    color: T.msFgOnAccent,
  },
  '&[data-variant="secondary"]': {
    color: T.msFg,
  },
  '&[data-variant="ghost"]': {
    color: T.msFgMuted,
  },
  '&[data-variant="danger"]': {
    background: T.msDanger,
    color: T.msFgOnAccent,
  },
  '&[data-block="true"]': {
    display: 'flex',
    justifyContent: 'center',
    '@supports (width: round(down, 100%, 1ch))': {
      width: raw('round(down, 100%, 1ch)'),
    },
  },
  '&:hover:not(:disabled)': {
    textDecoration: 'underline',
  },
  '&:focus-visible': {
    ...focusRing,
    background: T.msFg,
    color: T.msBg,
  },
  '&:disabled': {
    color: T.msFgSubtle,
    background: 'transparent',
    cursor: 'not-allowed',
    textDecoration: 'none',
  },
})

export function Button({
  variant = 'secondary',
  block = false,
  type = 'button',
  children,
  class: className,
  ...rest
}: ButtonProps) {
  return (
    <button
      {...rest}
      {...(className !== undefined ? { class: className } : {})}
      type={type}
      data-variant={variant}
      data-block={block ? 'true' : 'false'}
    >
      {msStyle(BUTTON)}
      {children}
    </button>
  )
}
