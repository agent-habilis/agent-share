import { tokens, Theme } from 'visage-style'
import type { ColorRole } from './theme/semantic.ts'
import { grid } from './theme/grid.ts'
import { semantic } from './theme/semantic.ts'

export const T = tokens({
  msFontSize: `${grid.fontSizePx}px`,
  msRow: `${grid.rowPx}px`,
  msMeasure: `${grid.measureCh}ch`,
  msBorderWidth: `${grid.borderPx}px`,
  msBg: semantic.bg,
  msBgSunken: semantic.bgSunken,
  msBgRaised: semantic.bgRaised,
  msBgSelected: semantic.bgSelected,
  msFg: semantic.fg,
  msFgMuted: semantic.fgMuted,
  msFgSubtle: semantic.fgSubtle,
  msFgOnAccent: semantic.fgOnAccent,
  msBorder: semantic.border,
  msBorderStrong: semantic.borderStrong,
  msAccent: semantic.accent,
  msAccentAlt: semantic.accentAlt,
  msSuccess: semantic.success,
  msWarning: semantic.warning,
  msDanger: semantic.danger,
  msInfo: semantic.info,
})

export const roleVar: Record<ColorRole, string> = {
  bg: T.msBg,
  bgSunken: T.msBgSunken,
  bgRaised: T.msBgRaised,
  bgSelected: T.msBgSelected,
  fg: T.msFg,
  fgMuted: T.msFgMuted,
  fgSubtle: T.msFgSubtle,
  fgOnAccent: T.msFgOnAccent,
  border: T.msBorder,
  borderStrong: T.msBorderStrong,
  accent: T.msAccent,
  accentAlt: T.msAccentAlt,
  success: T.msSuccess,
  warning: T.msWarning,
  danger: T.msDanger,
  info: T.msInfo,
}

export { Theme }
