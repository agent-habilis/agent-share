import { Style, css, raw } from 'visage-style'
import { oneRow } from '../../styles/mixins.ts'
import { T } from '../../tokens.ts'
import { glyphs } from '../../theme/glyphs.ts'

export interface SelectOption {
  value: string
  label: string
  disabled?: boolean
}

export interface SelectProps {
  options: SelectOption[]
  width?: number
  placeholder?: string
  value?: string
  defaultValue?: string
  disabled?: boolean
  name?: string
  id?: string
  class?: string
  onchange?: (event: Event) => void
  'aria-label'?: string
}

const WRAPPER = css({
  ...oneRow,
  position: 'relative',
  display: 'flex',
  alignItems: 'center',
  padding: raw('0 1ch'),
  background: T.msBgSunken,
  outline: raw('1px solid var(--ms-border)'),
  outlineOffset: 0,
  '&:focus-within': {
    outlineColor: T.msAccent,
  },
  '&:has(select:disabled)': {
    background: 'transparent',
    color: T.msFgSubtle,
  },
  '&[data-width="auto"]': {
    width: '100%',
    '@supports (width: round(down, 100%, 1ch))': {
      width: raw('round(down, 100%, 1ch)'),
    },
  },
  select: {
    ...oneRow,
    flex: 1,
    minWidth: 0,
    padding: 0,
    border: 0,
    outline: 0,
    background: 'transparent',
    color: 'inherit',
    cursor: 'pointer',
    appearance: 'none',
    '&:disabled': {
      cursor: 'not-allowed',
    },
    option: {
      background: T.msBgSunken,
      color: T.msFg,
    },
  },
  '.chevron': {
    flex: 'none',
    width: '2ch',
    textAlign: 'right',
    color: T.msFgMuted,
    pointerEvents: 'none',
  },
} as never)

export function Select({ options, width, placeholder, class: className, ...rest }: SelectProps) {
  return (
    <div
      {...(className !== undefined ? { class: className } : {})}
      data-width={width === undefined ? 'auto' : undefined}
      style={width !== undefined ? { width: `${width}ch` } : undefined}
    >
      {Style(WRAPPER)}
      <select {...rest}>
        {placeholder != null && (
          <option value="" disabled>
            {placeholder}
          </option>
        )}
        {options.map((option) => (
          <option key={option.value} value={option.value} disabled={option.disabled}>
            {option.label}
          </option>
        ))}
      </select>
      <span class="chevron" aria-hidden="true">
        {glyphs.chevron.down}
      </span>
    </div>
  )
}
