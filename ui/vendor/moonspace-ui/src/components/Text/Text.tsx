import { createElement } from 'visage-dom/element'
import type { Child } from 'visage-dom'
import { msCss as css, msStyle } from '../../styles/css.ts'
import { roleVar } from '../../tokens.ts'
import { theme } from '../../theme/theme.ts'
import type { ColorRole } from '../../theme/semantic.ts'
import type { FontWeight } from '../../theme/typography.ts'

export interface TextProps {
  as?: string
  color?: ColorRole
  weight?: FontWeight
  inverse?: boolean
  caps?: boolean
  truncate?: boolean
  children?: Child
  class?: string
  id?: string
  title?: string
  role?: string
  'aria-label'?: string
}

const TEXT = css({
  margin: 0,
  '&[data-caps="true"]': {
    textTransform: 'uppercase',
  },
  '&[data-truncate="true"]': {
    display: 'block',
    overflow: 'hidden',
    whiteSpace: 'nowrap',
    textOverflow: 'ellipsis',
  },
})

export function Text({
  as = 'span',
  color = 'fg',
  weight = 'regular',
  inverse = false,
  caps = false,
  truncate = false,
  children,
  class: className,
  ...rest
}: TextProps) {
  const style: Record<string, string> = {
    color: roleVar[color],
    fontWeight: String(theme.font.weight[weight]),
  }

  if (inverse) {
    style.background = roleVar[color]
    style.color = roleVar.bg
    style.padding = '0 1ch'
    style.margin = '0 -1ch'
  }

  return createElement(
    as,
    {
      ...rest,
      class: className,
      'data-caps': caps ? 'true' : undefined,
      'data-truncate': truncate ? 'true' : undefined,
      style,
    },
    [msStyle(TEXT), children ?? null],
  )
}
