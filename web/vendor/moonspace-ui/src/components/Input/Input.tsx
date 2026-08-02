import { Style, css, raw } from 'visage-style'
import { oneRow } from '../../styles/mixins.ts'
import { T } from '../../tokens.ts'

export interface InputProps {
  width?: number
  invalid?: boolean
  prefix?: string
  type?: string
  value?: string
  defaultValue?: string
  placeholder?: string
  disabled?: boolean
  name?: string
  id?: string
  class?: string
  oninput?: (event: Event) => void
  onchange?: (event: Event) => void
  'aria-label'?: string
}

const WRAPPER = css({
  ...oneRow,
  display: 'flex',
  alignItems: 'center',
  gap: 0,
  padding: raw('0 1ch'),
  background: T.msBgSunken,
  outline: raw('1px solid var(--ms-border)'),
  outlineOffset: 0,
  '&[data-invalid="true"]': {
    outlineColor: T.msDanger,
  },
  '&:focus-within': {
    outlineColor: T.msAccent,
  },
  '&[data-invalid="true"]:focus-within': {
    outlineColor: T.msDanger,
  },
  '&:has(input:disabled)': {
    background: 'transparent',
    outlineColor: T.msBorder,
  },
  '&[data-width="auto"]': {
    width: '100%',
    '@supports (width: round(down, 100%, 1ch))': {
      width: raw('round(down, 100%, 1ch)'),
    },
  },
  '.marker': {
    flex: 'none',
    width: '2ch',
    color: T.msFgSubtle,
    '&[data-invalid="true"]': {
      color: T.msDanger,
    },
  },
  input: {
    ...oneRow,
    flex: 1,
    minWidth: 0,
    padding: 0,
    border: 0,
    outline: 0,
    background: 'transparent',
    color: T.msFg,
    '&::placeholder': {
      color: T.msFgSubtle,
    },
    '&:disabled': {
      color: T.msFgSubtle,
      cursor: 'not-allowed',
    },
    '&[type=search]::-webkit-search-decoration, &[type=search]::-webkit-search-cancel-button': {
      appearance: 'none',
    },
    '&::-webkit-outer-spin-button, &::-webkit-inner-spin-button': {
      appearance: 'none',
      margin: 0,
    },
  },
} as never)

export function Input({ width, invalid = false, prefix, class: className, ...rest }: InputProps) {
  const marker = invalid ? '!' : prefix

  return (
    <div
      {...(className !== undefined ? { class: className } : {})}
      data-invalid={invalid ? 'true' : undefined}
      data-width={width === undefined ? 'auto' : undefined}
      style={width !== undefined ? { width: `${width}ch` } : undefined}
    >
      {Style(WRAPPER)}
      {marker != null && (
        <span class="marker" aria-hidden="true" data-invalid={invalid ? 'true' : undefined}>
          {marker}
        </span>
      )}
      <input aria-invalid={invalid || undefined} {...rest} />
    </div>
  )
}
