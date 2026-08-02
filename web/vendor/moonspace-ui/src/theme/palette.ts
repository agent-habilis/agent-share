/**
 * Tokyo Night Storm — raw palette.
 *
 * Values transcribed from folke/tokyonight.nvim (lua/tokyonight/colors/storm.lua).
 * These are the only hex literals in the design system.
 *
 * Components must never import from this file. Use `semantic.ts` instead — the
 * indirection is what lets us swap in another Tokyo Night variant, or an ANSI-16
 * terminal palette, without touching a single component.
 */
export const palette = {
  // Backgrounds, darkest to lightest.
  bgDark1: '#1b1e2d',
  bgDark: '#1f2335',
  bg: '#24283b',
  bgHighlight: '#292e42',

  // Foregrounds, brightest to dimmest.
  fg: '#c0caf5',
  fgDark: '#a9b1d6',
  dark5: '#737aa2',
  comment: '#565f89',
  dark3: '#545c7e',
  fgGutter: '#3b4261',
  terminalBlack: '#414868',

  // Hues.
  blue: '#7aa2f7',
  blue0: '#3d59a1',
  blue1: '#2ac3de',
  blue2: '#0db9d7',
  blue5: '#89ddff',
  blue6: '#b4f9f8',
  blue7: '#394b70',
  cyan: '#7dcfff',
  green: '#9ece6a',
  green1: '#73daca',
  green2: '#41a6b5',
  teal: '#1abc9c',
  yellow: '#e0af68',
  orange: '#ff9e64',
  red: '#f7768e',
  red1: '#db4b4b',
  magenta: '#bb9af7',
  magenta2: '#ff007c',
  purple: '#9d7cd8',

  // Git / diff.
  gitAdd: '#449dab',
  gitChange: '#6183bb',
  gitDelete: '#914c54',
} as const

export type Palette = typeof palette
export type PaletteName = keyof Palette
