import { createElement } from 'visage-dom/element'
import type { Child } from 'visage-dom'
import { msCss as css, msStyle } from '../../styles/css.ts'
import { roleVar } from '../../tokens.ts'
import { T } from '../../tokens.ts'
import { rows, snapWidth } from '../../styles/mixins.ts'
import type { BorderWeight } from '../../theme/glyphs.ts'
import type { ColorRole } from '../../theme/semantic.ts'
import { Divider } from '../Divider/Divider.tsx'
import { Text } from '../Text/Text.tsx'

export type BoxBorder = BorderWeight | 'none'

export interface BoxProps {
  as?: string
  border?: BoxBorder
  borderColor?: ColorRole
  background?: ColorRole
  padX?: number
  padY?: number
  width?: number | 'full' | 'measure' | 'auto'
  title?: Child
  unsnapped?: boolean
  children?: Child
  class?: string
  id?: string
}

const outlineFor = (border: BoxBorder) =>
  ({
    none: null,
    line: '1px solid',
    double: '3px double',
    thick: '2px solid',
  })[border]

const BOX = css({
  outlineOffset: 0,
})

const BOX_SNAP_FULL = css(snapWidth())
const BOX_SNAP_MEASURE = css(snapWidth('var(--ms-measure)'))
const BOX_UNSNAPPED = css({ width: '100%' })
const BOX_UNSNAPPED_MEASURE = css({ width: '100%', maxWidth: T.msMeasure })

export function Box({
  as = 'div',
  border = 'none',
  borderColor = 'border',
  background,
  padX = 1,
  padY = 0,
  width = 'full',
  title,
  unsnapped = false,
  children,
  class: className,
  ...rest
}: BoxProps) {
  const style: Record<string, string> = {
    padding: `${rows(padY)} ${padX}ch`,
    background: background ? roleVar[background] : 'transparent',
  }

  if (border !== 'none') {
    const rule = outlineFor(border)
    style.outline = `${rule} ${roleVar[borderColor]}`
  }

  if (width === 'auto') {
    style.width = 'max-content'
  } else if (typeof width === 'number') {
    style.width = `${width}ch`
  }

  let widthStyle = null
  if (width !== 'auto' && typeof width !== 'number') {
    if (unsnapped) {
      widthStyle = msStyle(width === 'measure' ? BOX_UNSNAPPED_MEASURE : BOX_UNSNAPPED)
    } else {
      widthStyle = msStyle(width === 'measure' ? BOX_SNAP_MEASURE : BOX_SNAP_FULL)
    }
  }

  return createElement(
    as,
    {
      ...rest,
      ...(className !== undefined ? { class: className } : {}),
      style,
    },
    [
      msStyle(BOX),
      widthStyle,
      title != null ? (
        <>
          <Text as="div" color="fgMuted" caps>
            {title}
          </Text>
          <Divider color={borderColor} />
        </>
      ) : null,
      children ?? null,
    ],
  )
}
