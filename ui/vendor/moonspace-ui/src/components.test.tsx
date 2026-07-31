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

test('Button includes bracket content rules', () => {
  render(Button({ children: 'OK' }), document.body)
  const btn = document.body.querySelector('button')
  expect(btn?.textContent?.endsWith('OK')).toBe(true)
  const css = [...document.body.querySelectorAll('style')].map((s) => s.textContent ?? '').join('')
  expect(css).toContain("'[ '")
  expect(css).toContain("' ]'")
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
