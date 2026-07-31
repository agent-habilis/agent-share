import { raw } from 'visage-style'
import { T } from '../tokens.ts'

/**
 * Grid math. Every dimension in the system goes through one of these.
 */

/** `n` cells of horizontal space. */
export const cells = (n: number): string => `${n}ch`

/** `n` rows of vertical space. */
export const rows = (n: number): string => `calc(${n} * var(--ms-row))`

/** Snap a percentage width down to a whole number of cells. */
export const snapWidth = (max?: string) => ({
  width: '100%' as const,
  ...(max ? { maxWidth: raw(max as never) } : {}),
  '@supports (width: round(down, 100%, 1ch))': {
    width: raw('round(down, 100%, 1ch)'),
    ...(max ? { maxWidth: raw(`min(${max}, round(down, 100%, 1ch))` as never) } : {}),
  },
})

/** Inverse video — swap foreground and background. */
export const inverse = {
  background: T.msFg,
  color: T.msBg,
}

/** Focus ring flush against the cell boundary. */
export const focusRing = {
  outline: raw('1px solid var(--ms-accent)'),
  outlineOffset: 0,
}

/** Applied to disabled controls. */
export const disabled = {
  color: T.msFgSubtle,
  borderColor: T.msBorder,
  cursor: 'not-allowed' as const,
  pointerEvents: 'none' as const,
}

/** Visually hidden but still announced by screen readers. */
export const srOnly = {
  position: 'absolute' as const,
  width: '1px',
  height: '1px',
  padding: 0,
  margin: '-1px',
  overflow: 'hidden' as const,
  clipPath: raw('inset(50%)'),
  whiteSpace: 'nowrap' as const,
  border: 0,
}

/** A single-row control on the baseline grid. */
export const oneRow = {
  height: raw(rows(1) as never),
  lineHeight: raw(rows(1) as never),
}
