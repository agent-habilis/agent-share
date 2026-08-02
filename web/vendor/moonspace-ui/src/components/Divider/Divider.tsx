import { msCss as css, msStyle } from '../../styles/css.ts'
import { roleVar } from '../../tokens.ts'
import type { BorderWeight } from '../../theme/glyphs.ts'
import type { ColorRole } from '../../theme/semantic.ts'

export interface DividerProps {
  weight?: BorderWeight
  orientation?: 'horizontal' | 'vertical'
  color?: ColorRole
  class?: string
  id?: string
}

const ruleFor = (weight: BorderWeight) =>
  ({
    line: '1px solid',
    double: '3px double',
    thick: '2px solid',
  })[weight]

const DIVIDER_H = css({
  border: 0,
  margin: 0,
  flexShrink: 0,
  display: 'flex',
  alignItems: 'center',
  width: '100%',
  height: 'var(--ms-row)',
  '&::before': {
    content: '""',
    flex: 1,
    borderTop: '1px solid currentColor',
  },
  '&[data-weight="double"]::before': {
    borderTop: '3px double currentColor',
  },
  '&[data-weight="thick"]::before': {
    borderTop: '2px solid currentColor',
  },
})

const DIVIDER_V = css({
  border: 0,
  margin: 0,
  flexShrink: 0,
  display: 'flex',
  justifyContent: 'center',
  width: '1ch',
  alignSelf: 'stretch',
  '&::before': {
    content: '""',
    height: '100%',
    borderLeft: '1px solid currentColor',
  },
  '&[data-weight="double"]::before': {
    borderLeft: '3px double currentColor',
  },
  '&[data-weight="thick"]::before': {
    borderLeft: '2px solid currentColor',
  },
})

export function Divider({
  weight = 'line',
  orientation = 'horizontal',
  color = 'border',
  class: className,
  ...rest
}: DividerProps) {
  const style = { color: roleVar[color] }
  const isVertical = orientation === 'vertical'

  if (isVertical) {
    return (
      <div
        role="separator"
        aria-orientation={orientation}
        class={className}
        data-weight={weight}
        style={style}
        {...rest}
      >
        {msStyle(DIVIDER_V)}
      </div>
    )
  }

  return (
    <hr class={className} data-weight={weight} style={style} {...rest}>
      {msStyle(DIVIDER_H)}
    </hr>
  )
}
