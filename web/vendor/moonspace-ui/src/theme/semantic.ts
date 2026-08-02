import { palette } from './palette.ts'

/**
 * Semantic color roles — the only color names components are allowed to use.
 *
 * The set is deliberately small. A monospace system has no type scale to lean on,
 * so color is doing more hierarchy work than usual; keeping the vocabulary tight
 * is what stops it from sprawling into an arbitrary palette-by-another-name.
 *
 * Every role here has a counterpart in `ansi.ts`. If you add a role, add its ANSI
 * mapping too — that constraint is what keeps the system portable to a terminal.
 */
export const semantic = {
  /** Page background. */
  bg: palette.bg,
  /** Recessed surfaces: code blocks, table headers, inset wells. */
  bgSunken: palette.bgDark,
  /** Raised surfaces: menus, popovers, hovered rows. */
  bgRaised: palette.bgHighlight,
  /** Active selection. Pairs with `fg` — this is the "inverse video" background. */
  bgSelected: palette.blue7,

  /** Body text. */
  fg: palette.fg,
  /** Secondary text: labels, captions, inactive tabs. */
  fgMuted: palette.dark5,
  /** Tertiary text: placeholders, disabled controls, the dim half of a progress bar. */
  fgSubtle: palette.comment,
  /** Text drawn on top of an `accent` fill. */
  fgOnAccent: palette.bgDark,

  /** Default rules and component chrome. */
  border: palette.fgGutter,
  /** Emphasized rules: focused fields, active panel edges. */
  borderStrong: palette.terminalBlack,

  /** Primary interactive color: focus, links, selected tab. */
  accent: palette.blue,
  /** Secondary interactive color: visited, alternate emphasis. */
  accentAlt: palette.magenta,

  /** Status. */
  success: palette.green,
  warning: palette.yellow,
  danger: palette.red,
  info: palette.cyan,
} as const

export type Semantic = typeof semantic
export type ColorRole = keyof Semantic
