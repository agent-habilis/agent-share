import { test, expect, beforeEach } from 'bun:test'
import {
  asyncComponent,
  component,
  context,
  render,
  signal,
  tags,
  flushSync,
} from '../index.ts'
import type { Signal } from '../index.ts'

const tick = (ms = 0) => new Promise((resolve) => setTimeout(resolve, ms))

const { div, span, ul, li } = tags

let host: HTMLElement

beforeEach(() => {
  document.body.innerHTML = ''
  host = document.createElement('div')
  document.body.appendChild(host)
})

// ---------------------------------------------------------------------------
// Lookup
// ---------------------------------------------------------------------------

test('a descendant reads what an ancestor provided', () => {
  const Theme = context<string>('theme')

  const Leaf = component(function* (_props, ctx) {
    const theme = ctx.inject(Theme)
    yield () => span(theme)
  })

  const Middle = component(function* () {
    yield () => div(Leaf())
  })

  const App = component(function* (_props, ctx) {
    ctx.provide(Theme, 'dark')
    yield () => div(Middle())
  })

  render(App(), host)
  expect(host.textContent).toBe('dark')
})

test('the nearest provider wins', () => {
  const Theme = context<string>('theme')

  const Leaf = component(function* (_props, ctx) {
    yield () => span(ctx.inject(Theme))
  })

  const Inner = component(function* (_props, ctx) {
    ctx.provide(Theme, 'inner')
    yield () => div(Leaf())
  })

  const Outer = component(function* (_props, ctx) {
    ctx.provide(Theme, 'outer')
    yield () => div(Leaf(), Inner())
  })

  render(Outer(), host)
  // First leaf sees the outer value, the one below Inner sees the inner value.
  expect(host.textContent).toBe('outerinner')
})

test('siblings do not see one another values', () => {
  const Slot = context<string>('slot')

  const Leaf = component(function* (_props, ctx) {
    yield () => span(ctx.inject(Slot))
  })

  const Branch = component<{ value: string }>(function* (props, ctx) {
    ctx.provide(Slot, props.value)
    yield () => div(Leaf())
  })

  const App = component(function* () {
    yield () => div(Branch({ value: 'a' }), Branch({ value: 'b' }))
  })

  render(App(), host)
  expect(host.textContent).toBe('ab')
})

// ---------------------------------------------------------------------------
// Defaults and failure
// ---------------------------------------------------------------------------

test('falls back to the token default', () => {
  const Theme = context<string>('theme', 'light')

  const App = component(function* (_props, ctx) {
    yield () => div(ctx.inject(Theme))
  })

  render(App(), host)
  expect(host.textContent).toBe('light')
})

test('a default of undefined is still a default', () => {
  const Maybe = context<string | undefined>('maybe', undefined)

  const App = component(function* (_props, ctx) {
    yield () => div(String(ctx.inject(Maybe)))
  })

  render(App(), host)
  expect(host.textContent).toBe('undefined')
})

test('injecting an unprovided token without a default throws', () => {
  const Missing = context<string>('router')

  const App = component(function* (_props, ctx) {
    yield () => div(ctx.inject(Missing))
  })

  expect(() => render(App(), host)).toThrow(/no value provided for context "router"/)
})

test('two tokens with the same name do not collide', () => {
  const a = context<string>('dup', 'from-a')
  const b = context<string>('dup', 'from-b')

  const App = component(function* (_props, ctx) {
    ctx.provide(a, 'provided-a')
    yield () => div(ctx.inject(a), '/', ctx.inject(b))
  })

  render(App(), host)
  expect(host.textContent).toBe('provided-a/from-b')
})

// ---------------------------------------------------------------------------
// Reactivity comes from the value, not from context
// ---------------------------------------------------------------------------

test('a signal-valued context propagates updates to readers', () => {
  const Theme = context<Signal<string>>('theme')
  const theme = signal('dark')

  const Leaf = component(function* (_props, ctx) {
    const value = ctx.inject(Theme)
    // Read inside the loop, so it is re-read on every resume and tracked.
    yield () => span(value.value)
  })

  const App = component(function* (_props, ctx) {
    ctx.provide(Theme, theme)
    yield () => div(Leaf())
  })

  render(App(), host)
  expect(host.textContent).toBe('dark')

  theme.value = 'light'
  flushSync()
  // Only Leaf read the signal, so only Leaf resumed.
  expect(host.textContent).toBe('light')
})

// ---------------------------------------------------------------------------
// The parent link has to survive re-renders, not just the first mount
// ---------------------------------------------------------------------------

test('a component mounted during a later re-render still finds the value', () => {
  const Theme = context<string>('theme')

  const Leaf = component(function* (_props, ctx) {
    yield () => span(ctx.inject(Theme))
  })

  const show = signal(false)
  const App = component(function* (_props, ctx) {
    ctx.provide(Theme, 'dark')
    // Leaf does not exist on the first render; it is mounted from a commit.
    yield () => div(show.value ? Leaf() : 'empty')
  })

  render(App(), host)
  expect(host.textContent).toBe('empty')

  show.value = true
  flushSync()
  expect(host.textContent).toBe('dark')
})

test('items added to a keyed list find the value', () => {
  const Prefix = context<string>('prefix')

  const Item = component<{ n: number }>(function* (props, ctx) {
    const prefix = ctx.inject(Prefix)
    yield () => li(`${prefix}${props.n}`)
  })

  const items = signal<number[]>([1])
  const App = component(function* (_props, ctx) {
    ctx.provide(Prefix, '#')
    yield () => ul(items.value.map((n) => Item({ key: n, n })))
  })

  render(App(), host)
  expect(host.textContent).toBe('#1')

  items.value = [1, 2, 3]
  flushSync()
  expect(host.textContent).toBe('#1#2#3')
})

test('an async component provides to children it mounts after awaiting', async () => {
  const Theme = context<string>('theme')

  const Leaf = component(function* (_props, ctx) {
    yield () => span(ctx.inject(Theme))
  })

  const App = asyncComponent(async function* (_props, ctx) {
    ctx.provide(Theme, 'async-dark')
    yield div('loading')
    await tick(5)
    // Mounted from a commit that happens after an await, where the resume
    // window has already closed.
    while (true) yield div(Leaf())
  })

  // An async component mounts a hole and fills it as values arrive, so even
  // the first yield lands a tick later.
  render(App(), host)
  await tick(0)
  expect(host.textContent).toBe('loading')

  await tick(30)
  expect(host.textContent).toBe('async-dark')
})

test('provide is scoped to the subtree and gone after unmount', () => {
  const Theme = context<string>('theme', 'default')

  const Leaf = component(function* (_props, ctx) {
    yield () => span(ctx.inject(Theme))
  })

  const Provider = component(function* (_props, ctx) {
    ctx.provide(Theme, 'scoped')
    yield () => div(Leaf())
  })

  const inside = signal(true)
  const App = component(function* () {
    yield () => div(inside.value ? Provider() : Leaf())
  })

  render(App(), host)
  expect(host.textContent).toBe('scoped')

  inside.value = false
  flushSync()
  expect(host.textContent).toBe('default')
})
