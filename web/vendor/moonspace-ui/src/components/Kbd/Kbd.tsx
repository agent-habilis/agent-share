import { Style, css, raw } from 'visage-style'
import { oneRow } from '../../styles/mixins.ts'
import { T } from '../../tokens.ts'

export interface KbdProps {
  children?: import('visage-dom').Child
  class?: string
  id?: string
}

const KBD = css({
  ...oneRow,
  display: 'inline-flex',
  alignItems: 'center',
  padding: raw('0 1ch'),
  background: T.msBgRaised,
  color: T.msFg,
  whiteSpace: 'nowrap',
} as never)

export function Kbd({ children, class: className, ...rest }: KbdProps) {
  return (
    <kbd {...rest} {...(className !== undefined ? { class: className } : {})}>
      {Style(KBD)}
      {children}
    </kbd>
  )
}
