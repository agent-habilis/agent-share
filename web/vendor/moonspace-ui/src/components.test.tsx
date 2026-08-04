import { test, expect, beforeEach } from 'bun:test'
import { render } from 'visage-dom'
import { Theme, T } from './tokens.ts'
import { Badge } from './components/Badge/Badge.tsx'
import { Button } from './components/Button/Button.tsx'
import { Text } from './components/Text/Text.tsx'
import { Spinner } from './components/Spinner/Spinner.tsx'

beforeEach(() => {
  document.body.innerHTML = ''
  render(Theme(T), document.body)
})

test('Badge outline includes bracket content rules', () => {
  render(Badge({ variant: 'outline', children: 'live' }), document.body)
  const badge = document.body.querySelector('[data-variant="outline"]')
  expect(badge?.textContent?.endsWith('live')).toBe(true)
  const css = [...document.body.querySelectorAll('style')].map((s) => s.textContent ?? '').join('')
  expect(css).toContain("'‹'")
  expect(css).toContain("'›'")
})

test('Button renders a padded box with a zero-layout edge', () => {
  render(Button({ children: 'OK' }), document.body)
  const btn = document.body.querySelector('button')
  expect(btn?.textContent?.endsWith('OK')).toBe(true)
  // The button's own scoped rules, not every style on the page: the `not`
  // below would otherwise trip over `--ms-border-width` in the theme's :root.
  const css = btn?.querySelector('style')?.textContent ?? ''
  // One cell of horizontal padding, and no vertical padding — a cell is `1ch`
  // wide but a row tall, so padding the block axis would take the control off
  // the grid.
  expect(css).toContain('padding:0 1ch')
  // The edge is an outline, never a border: an outline costs no layout, so the
  // content box stays a full row and the width stays a whole number of cells.
  expect(css).toContain('outline:var(--ms-border-width) solid transparent')
  // And the UA's `2px outset` on a bare <button> has to be zeroed, or it eats
  // the content box the outline was chosen to preserve.
  expect(css).toContain('border:0')
  expect(css).not.toContain('border-width:')
  expect(css).not.toContain('border-color:')
})

test('Text applies color role via inline style', () => {
  render(Text({ color: 'accent', children: 'hello' }), document.body)
  const el = document.body.querySelector('span[style*="--ms-accent"]')
  expect(el?.textContent?.endsWith('hello')).toBe(true)
})

test('Spinner exposes role=status', () => {
  render(Spinner({ label: 'Loading' }), document.body)
  expect(document.body.querySelector('[role=status]')).not.toBeNull()
})

test('middleTruncate keeps head and tail', async () => {
  const { middleTruncate } = await import('./hooks/middleTruncate.ts')
  expect(middleTruncate('/Users/dev/project/file.tsx', 20)).toBe('/Users/dev…/file.tsx')
})
