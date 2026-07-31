import { Style, css } from 'visage-style'

type Decl = Record<string, unknown>

/** Hoist styles when nested selectors or token spreads exceed CheckStyle's inference. */
export function msCss(declarations: Decl): Decl {
  return css(declarations as never) as Decl
}

/** Scoped style element with the same relaxed checking as msCss. */
export function msStyle(declarations: Decl) {
  return Style(declarations as never)
}
