/**
 * The toast takes the top bar's one row while it is up, so the two things worth
 * pinning are that it says what went wrong and that it can never need a second
 * row — a message that wrapped would push the whole page down, which is the bug
 * this component replaced.
 */

import { test, expect, beforeEach, afterEach } from 'bun:test'
import { render, flushSync } from 'visage-dom'
import type { Root } from 'visage-dom'

import { Toast } from './index.tsx'

let host: HTMLElement
let root: Root | null = null
let closes = 0

beforeEach(() => {
  document.body.innerHTML = ''
  host = document.createElement('div')
  document.body.appendChild(host)
  closes = 0
})

afterEach(() => {
  root?.unmount()
  root = null
})

function mount(tone: 'error' | 'warning', message: string): void {
  root = render(
    Toast({
      tone,
      message,
      onClose: () => {
        closes += 1
      },
    }),
    host,
  )
  flushSync()
}

function said(): HTMLElement | null {
  return host.querySelector('[data-testid="toast"]')
}

test('says what went wrong, and lets it be copied', () => {
  const message =
    "Failed to execute 'showSaveFilePicker' on 'Window': Must be handling a user gesture."
  mount('error', message)

  // `toEndWith`, not `toBe`: the style system inlines a `<style>` inside the
  // element, so its `textContent` opens with a stylesheet.
  expect(said()?.textContent).toEndWith(message)
  // Destined for a bug report, so it is one of the few things in this app that
  // may be selected. See `.selectable` in `app.css`.
  expect(said()?.classList.contains('selectable')).toBe(true)
  // Truncated rather than wrapped, and the tooltip is what gives the rest back.
  expect(said()?.dataset['truncate']).toBe('true')
  expect(said()?.getAttribute('title')).toBe(message)
})

test('red for a failure, yellow for a notice', () => {
  mount('error', 'the peer went away')
  expect(said()?.style.getPropertyValue('--ms-text-color')).toBe('var(--danger)')
  root?.unmount()
  root = null

  mount('warning', '3 entries hidden')
  expect(said()?.style.getPropertyValue('--ms-text-color')).toBe('var(--warning)')
})

test('the tone is a word too, not only a colour', () => {
  mount('warning', '3 entries hidden')

  // The badge stands where the brand does on every other page, so it is the
  // first thing on the row rather than something to hunt for.
  const badge = host.querySelector('[data-variant="solid"]')
  expect(badge?.textContent).toEndWith('warning')
})

test('the close button hands the bar back', () => {
  const close = () =>
    [...host.querySelectorAll('button')].find((button) =>
      button.textContent?.toLowerCase().endsWith('close'),
    )

  mount('error', 'the peer went away')

  expect(close()).toBeDefined()
  close()?.click()

  expect(closes).toBe(1)
})
