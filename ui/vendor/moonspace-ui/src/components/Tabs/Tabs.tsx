import { component, signal } from 'visage-dom'
import type { Child } from 'visage-dom'
import { Style, css, raw } from 'visage-style'
import { oneRow } from '../../styles/mixins.ts'
import { T } from '../../tokens.ts'

export interface Tab {
  id: string
  label: string
  content?: Child
  disabled?: boolean
}

export interface TabsProps {
  tabs: Tab[]
  value?: string
  onValueChange?: (id: string) => void
  defaultValue?: string
  class?: string
}

let nextTabsId = 0

const LIST = css({
  ...oneRow,
  display: 'flex',
  gap: 0,
} as never)

const TRIGGER = css({
  ...oneRow,
  padding: raw('0 2ch'),
  border: 0,
  background: 'transparent',
  color: T.msFgMuted,
  cursor: 'pointer',
  whiteSpace: 'nowrap',
  '&[data-selected="true"]': {
    background: T.msFg,
    color: T.msBg,
  },
  '&:hover:not(:disabled):not([data-selected="true"])': {
    color: T.msFg,
  },
  '&:focus-visible': {
    outline: raw('1px solid var(--ms-accent)'),
    outlineOffset: '-1px',
  },
  '&:disabled': {
    color: T.msFgSubtle,
    cursor: 'not-allowed',
  },
} as never)

const PANEL = css({
  padding: raw('var(--ms-row) 0 0'),
} as never)

export const Tabs = component<TabsProps>(function* (props) {
  const baseId = `ms-tabs-${++nextTabsId}`
  const internal = signal(props.defaultValue ?? props.tabs[0]?.id ?? '')

  const select = (id: string) => {
    if (props.value === undefined) internal.value = id
    props.onValueChange?.(id)
  }

  const move = (direction: 1 | -1) => {
    const enabled = props.tabs.filter((tab) => !tab.disabled)
    const selected = props.value ?? internal.value
    const index = enabled.findIndex((tab) => tab.id === selected)
    const next = enabled[(index + direction + enabled.length) % enabled.length]
    if (next) select(next.id)
  }

  yield () => {
    const selected = props.value ?? internal.value
    const active = props.tabs.find((tab) => tab.id === selected)

    return (
      <div {...(props.class !== undefined ? { class: props.class } : {})}>
        <div role="tablist">
          {Style(LIST)}
          {props.tabs.map((tab) => (
            <button
              key={tab.id}
              id={`${baseId}-tab-${tab.id}`}
              role="tab"
              type="button"
              data-selected={tab.id === selected ? 'true' : 'false'}
              aria-selected={tab.id === selected}
              aria-controls={`${baseId}-panel-${tab.id}`}
              tabIndex={tab.id === selected ? 0 : -1}
              disabled={tab.disabled}
              onclick={() => select(tab.id)}
              onkeydown={(event) => {
                if (event.key === 'ArrowRight') {
                  event.preventDefault()
                  move(1)
                } else if (event.key === 'ArrowLeft') {
                  event.preventDefault()
                  move(-1)
                }
              }}
            >
              {Style(TRIGGER)}
              {tab.label}
            </button>
          ))}
        </div>

        {active?.content != null && (
          <div
            id={`${baseId}-panel-${active.id}`}
            role="tabpanel"
            aria-labelledby={`${baseId}-tab-${active.id}`}
            tabIndex={0}
          >
            {Style(PANEL)}
            {active.content}
          </div>
        )}
      </div>
    )
  }
})
