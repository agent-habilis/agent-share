/**
 * moonspace-ui — a monospace design system.
 *
 * One font, one size, one grid.
 */

// Theme — plain data, no React or CSS dependency. Safe for a TUI renderer to import.
export * from './theme/index.ts'

// Tokens
export { T, roleVar, Theme } from './tokens.ts'

// Styles
export {
  cells,
  rows,
  snapWidth,
  inverse,
  focusRing,
  disabled,
  srOnly,
  oneRow,
} from './styles/mixins.ts'

// Hooks
export { middleTruncate } from './hooks/middleTruncate.ts'

// Primitives
export * from './components/Box/index.ts'
export * from './components/Text/index.ts'
export * from './components/Stack/index.ts'
export * from './components/Divider/index.ts'

// Controls
export * from './components/Button/index.ts'
export * from './components/Input/index.ts'
export * from './components/Select/index.ts'
export * from './components/Checkbox/index.ts'
export * from './components/Radio/index.ts'

// Display
export * from './components/Badge/index.ts'
export * from './components/StatusDot/index.ts'
export * from './components/Table/index.ts'
export * from './components/ProgressBar/index.ts'
export * from './components/Spinner/index.ts'
export * from './components/MiddleTruncate/index.ts'
export * from './components/Kbd/index.ts'

// Feedback and navigation
export * from './components/Note/index.ts'
export * from './components/Tabs/index.ts'
