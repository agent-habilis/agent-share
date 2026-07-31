import type { ColorRole } from './semantic.ts'

/**
 * The 16 slots every terminal has, whatever its color depth.
 */
export type AnsiColor =
  | 'black'
  | 'red'
  | 'green'
  | 'yellow'
  | 'blue'
  | 'magenta'
  | 'cyan'
  | 'white'
  | 'brightBlack'
  | 'brightRed'
  | 'brightGreen'
  | 'brightYellow'
  | 'brightBlue'
  | 'brightMagenta'
  | 'brightCyan'
  | 'brightWhite'
  | 'default'

/**
 * Every semantic role, collapsed onto the ANSI-16 palette.
 *
 * The web build never reads this. It exists for two reasons:
 *
 * 1. The planned TUI renderer gets a theme for free — same role names, terminal values.
 * 2. It forces a design constraint at authoring time. If a new role can't be expressed
 *    in 16 colors, it's a role that will not survive the port, and we'd rather find
 *    that out while adding the token than while rewriting the component.
 *
 * Note the collisions: `bgSunken`/`bgRaised` both fall back to `default`, and
 * `fgMuted`/`fgSubtle` both land on `brightBlack`. That is precisely why depth and
 * state must never be communicated by color alone — see the inverse-video and glyph
 * rules in the component docs.
 */
export const ansi: Record<ColorRole, AnsiColor> = {
  bg: 'default',
  bgSunken: 'default',
  bgRaised: 'default',
  bgSelected: 'blue',

  fg: 'white',
  fgMuted: 'brightBlack',
  fgSubtle: 'brightBlack',
  fgOnAccent: 'black',

  border: 'brightBlack',
  borderStrong: 'white',

  accent: 'brightBlue',
  accentAlt: 'brightMagenta',

  success: 'green',
  warning: 'yellow',
  danger: 'red',
  info: 'cyan',
}
