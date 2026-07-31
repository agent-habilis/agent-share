import { createElement } from 'visage-dom/element'
import type { Child } from 'visage-dom'
import { Style, css, raw } from 'visage-style'

type Align = 'start' | 'center' | 'end' | 'stretch' | 'baseline'
type Justify = 'start' | 'center' | 'end' | 'between' | 'around'

const alignMap: Record<Align, string> = {
  start: 'flex-start',
  center: 'center',
  end: 'flex-end',
  stretch: 'stretch',
  baseline: 'baseline',
}

const justifyMap: Record<Justify, string> = {
  start: 'flex-start',
  center: 'center',
  end: 'flex-end',
  between: 'space-between',
  around: 'space-around',
}

export interface StackProps {
  as?: string
  direction?: 'row' | 'column'
  gap?: number
  align?: Align
  justify?: Justify
  wrap?: boolean
  snap?: boolean
  children?: Child
  class?: string
  id?: string
}

const needsSnap = (justify: Justify, align: Align) =>
  justify !== 'start' || align === 'center' || align === 'end'

const STACK = css({
  display: 'flex',
  '&[data-wrap="true"]': { flexWrap: 'wrap' },
  '&[data-wrap="false"]': { flexWrap: 'nowrap' },
  '&[data-snap="true"]': {
    '@supports (width: round(down, 100%, 1ch))': {
      width: raw('round(down, 100%, 1ch)'),
    },
  },
} as never)

export function Stack({
  as = 'div',
  direction = 'column',
  gap = 0,
  align = 'stretch',
  justify = 'start',
  wrap = false,
  snap,
  children,
  class: className,
  ...rest
}: StackProps) {
  const style: Record<string, string> = {
    flexDirection: direction,
    alignItems: alignMap[align],
    justifyContent: justifyMap[justify],
    gap: direction === 'row' ? `${gap}ch` : `calc(${gap} * var(--ms-row))`,
  }

  return createElement(
    as,
    {
      ...rest,
      ...(className !== undefined ? { class: className } : {}),
      'data-wrap': wrap ? 'true' : 'false',
      'data-snap': (snap ?? needsSnap(justify, align)) ? 'true' : 'false',
      style,
    },
    [Style(STACK), children ?? null],
  )
}
