import { ansi } from './ansi.ts'
import { boxDrawing, glyphs } from './glyphs.ts'
import { grid } from './grid.ts'
import { palette } from './palette.ts'
import { semantic } from './semantic.ts'
import { fontFamily, fontFeatureSettings, fontWeight } from './typography.ts'

/**
 * The assembled theme, handed to styled-components' `ThemeProvider`.
 *
 * This is a plain object with no React or CSS dependency — deliberately. The
 * planned TUI renderer can import it verbatim and read `color`, `ansi`, `glyphs`
 * and `boxDrawing` without pulling in any of the web layer.
 */
export const theme = {
  name: 'tokyo-night-storm',
  /** Semantic roles. This is what components read. */
  color: semantic,
  /** Raw palette. Escape hatch for docs and swatches — not for components. */
  palette,
  /** Terminal fallback for each semantic role. Unused by the web build. */
  ansi,
  grid,
  glyphs,
  boxDrawing,
  font: {
    family: fontFamily,
    weight: fontWeight,
    featureSettings: fontFeatureSettings,
  },
} as const

export type Theme = typeof theme
