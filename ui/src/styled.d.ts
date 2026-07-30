// moonspace-ui augments styled-components' `DefaultTheme` in its own
// `src/theme/styled.d.ts`, but a module augmentation only applies to programs
// that actually include the declaring file — and consuming the package by path
// does not pull it in. Re-declaring it here is what makes every `props.theme`
// interpolation inside moonspace-ui typecheck against the real token set
// instead of an empty `DefaultTheme`.
import 'styled-components'
import type { theme } from 'moonspace-ui'

declare module 'styled-components' {
  export interface DefaultTheme extends Omit<typeof theme, never> {}
}
