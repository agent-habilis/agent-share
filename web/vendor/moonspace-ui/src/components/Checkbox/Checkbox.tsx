import type { Child } from 'visage-dom'
import { msCss as css, msStyle } from '../../styles/css.ts'
import { oneRow, srOnly } from '../../styles/mixins.ts'
import { T } from '../../tokens.ts'
import { glyphs } from '../../theme/glyphs.ts'

export interface CheckboxProps {
  indeterminate?: boolean
  checked?: boolean
  disabled?: boolean
  name?: string
  value?: string
  id?: string
  class?: string
  onchange?: (event: Event) => void
  children?: Child
}

const LABEL = css({
  ...oneRow,
  display: 'inline-flex',
  alignItems: 'center',
  gap: '1ch',
  cursor: 'pointer',
  '&:has(input:disabled)': {
    color: T.msFgSubtle,
    cursor: 'not-allowed',
  },
  '.native': srOnly,
  '.marker': {
    flex: 'none',
    width: '3ch',
    color: T.msFg,
  },
  'input:focus-visible + .marker': {
    background: T.msFg,
    color: T.msBg,
  },
  'input:checked + .marker': {
    color: T.msAccent,
  },
  'input:disabled + .marker': {
    color: T.msFgSubtle,
  },
})

export function Checkbox({
  indeterminate = false,
  checked,
  children,
  class: className,
  ...rest
}: CheckboxProps) {
  const state = indeterminate
    ? glyphs.checkbox.mixed
    : checked
      ? glyphs.checkbox.on
      : glyphs.checkbox.off

  return (
    <label class={className}>
      {msStyle(LABEL)}
      <input
        class="native"
        type="checkbox"
        checked={checked}
        aria-checked={indeterminate ? 'mixed' : undefined}
        {...rest}
      />
      <span class="marker" aria-hidden="true">
        [{state}]
      </span>
      {children != null && <span>{children}</span>}
    </label>
  )
}
