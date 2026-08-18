/**
 * Row gestures: one click selects, two open.
 *
 * The double-click accelerator relies on the clicks underneath it having
 * already moved the selection — `onPreview` reads the selection rather than
 * being handed a node — so these pin the ordering as much as the wiring.
 */

import { test, expect, beforeEach, afterEach } from 'bun:test'
import { render, flushSync } from 'visage-dom'
import type { Root } from 'visage-dom'

import { ColumnView } from './index.tsx'
import type { DirNode } from 'agent-share-core/tree'

let host: HTMLElement
let root: Root | null = null
let selected: string[][] = []
let previews = 0

const root_: DirNode = {
  kind: 'dir',
  name: '',
  path: '',
  children: [
    { kind: 'file', name: 'note.txt', path: 'note.txt', index: 0, size: 44, mtime: 0 },
    {
      kind: 'dir',
      name: 'docs',
      path: 'docs',
      children: [
        { kind: 'file', name: 'deep.md', path: 'docs/deep.md', index: 1, size: 9, mtime: 0 },
      ],
    },
  ],
}

const held = new Set<number>()
const noop = () => undefined

beforeEach(() => {
  document.body.innerHTML = ''
  host = document.createElement('div')
  document.body.appendChild(host)
  selected = []
  previews = 0
})

afterEach(() => {
  root?.unmount()
  root = null
})

function mount(previewDisabled = false): void {
  root = render(
    ColumnView({
      root: root_,
      path: [],
      onPathChange: (next) => {
        selected.push(next)
      },
      onDownload: noop,
      held,
      onSeed: noop,
      onPreview: () => {
        previews += 1
      },
      previewDisabled,
    }),
    host,
  )
  flushSync()
}

/** The row whose name cell reads `name`. */
function row(name: string): HTMLElement {
  const hit = Array.from(host.querySelectorAll('[role="button"]')).find((el) =>
    el.getAttribute('title') === name || el.textContent?.includes(name),
  )
  if (!hit) throw new Error(`no row for "${name}"`)
  return hit as HTMLElement
}

const click = (el: HTMLElement) =>
  el.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true }))
const dblclick = (el: HTMLElement) =>
  el.dispatchEvent(new MouseEvent('dblclick', { bubbles: true, cancelable: true }))

test('one click selects and does not open', () => {
  mount()
  click(row('note.txt'))
  flushSync()

  expect(selected).toEqual([['note.txt']])
  expect(previews).toBe(0)
})

test('double-clicking a file opens it', () => {
  mount()
  const target = row('note.txt')
  // A real double-click delivers both clicks before the dblclick, which is
  // what leaves the row selected for `onPreview` to read.
  click(target)
  click(target)
  dblclick(target)
  flushSync()

  expect(selected[selected.length - 1]).toEqual(['note.txt'])
  expect(previews).toBe(1)
})

test('double-clicking a folder opens nothing — the first click already did', () => {
  mount()
  const target = row('docs')
  click(target)
  dblclick(target)
  flushSync()

  expect(selected[selected.length - 1]).toEqual(['docs'])
  expect(previews).toBe(0)
})

test('the accelerator is off whenever the button would be', () => {
  mount(true)
  const target = row('note.txt')
  click(target)
  dblclick(target)
  flushSync()

  expect(previews).toBe(0)
})
