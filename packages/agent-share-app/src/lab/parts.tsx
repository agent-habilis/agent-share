/**
 * The pieces every lab panel is built from.
 *
 * # Why some of this is imperative
 *
 * Two Rust harnesses drive this page through the DOM, and they are most of the
 * reason it exists: `tasks/src/e2e.rs` produces a share from a tab and opens it
 * with the CLI, and `tasks/src/bench/browser.rs` runs the browser cells of the
 * performance matrix. Both address the controls by the ids committed here, set
 * `.value` directly — dispatching no event — and read the log back out of the
 * `<pre>`. Two consequences run through the panels:
 *
 * 1. A click handler reads the live control (`el.value`), never a signal. A
 *    signal would still hold what the page last saw a human type.
 * 2. The log is written into its node, not rendered from a signal. The bench
 *    clears `#rx-log` itself between repeats; a rendered log would repaint the
 *    cleared history on the next line and the harness would parse the previous
 *    repeat's report as this one's.
 */

import { Stack, Text, cells, rows, t } from 'moonspace-dom'
import type { Child } from 'visage-dom/types'

/** How tall a log pane is, in rows — the same twelve `/info` gives its own. */
const LOG_ROWS = 12

/** How tall a ticket box is, in rows. Enough for a ticket to wrap once. */
const TICKET_ROWS = 4

export type Log = (...parts: unknown[]) => void

export function jsError(error: unknown): string {
  if (error instanceof Error) return error.message
  if (typeof error === 'string') return error
  try {
    return JSON.stringify(error)
  } catch {
    return String(error)
  }
}

/**
 * A log pane, and the function that writes to it.
 *
 * The element is built once, when the panel is constructed, rather than in the
 * render thunk — so the lines appended to it survive every repaint the panel's
 * signals cause. It carries no children of its own, which is what keeps the
 * reconciler out of the text: an element described with no children is never
 * diffed against the nodes it actually holds.
 */
export function createLog(id: string): { view: Child; log: Log } {
  let node: HTMLPreElement | null = null

  const view = (
    <pre
      id={id}
      ref={(el) => {
        node = el
      }}
      style={{
        margin: 0,
        padding: `0 ${cells(1)}`,
        height: rows(LOG_ROWS),
        overflow: 'auto',
        background: t.bgSunken,
        color: t.success,
        // `<pre>` is the one element the reset leaves on the UA's font, and the
        // UA's is 13px monospace — a size the grid knows nothing about.
        fontFamily: 'inherit',
        fontSize: 'inherit',
        lineHeight: 'var(--ms-row)',
        whiteSpace: 'pre-wrap',
        wordBreak: 'break-word',
      }}
    />
  )

  const log: Log = (...parts: unknown[]) => {
    const line = parts
      .map((part) => {
        if (part instanceof Error) return part.stack || part.message
        if (typeof part === 'string') return part
        try {
          return JSON.stringify(part)
        } catch {
          return String(part)
        }
      })
      .join(' ')
    const stamp = new Date().toISOString().slice(11, 23)
    if (node) {
      node.textContent += `[${stamp}] ${line}\n`
      node.scrollTop = node.scrollHeight
    }
    console.log('[lab]', ...parts)
  }

  return { view, log }
}

/** A control with its name beside it. */
export function Field(props: { label: string; children?: Child }) {
  return (
    <Stack direction="row" gap={1} align="center">
      <Text color="fgMuted">{props.label}</Text>
      {props.children}
    </Stack>
  )
}

/**
 * A ticket, in a box.
 *
 * A `<textarea>` rather than moonspace's `Input`, because a ticket is a few
 * hundred characters and a one-row field shows a tenth of it. `value` is left
 * unset for the boxes a person types into: passing one would mean the render
 * owns the text, and the harness writes to these directly.
 */
export function TicketBox(props: {
  id: string
  placeholder: string
  value?: string
  readOnly?: boolean
  ref?: (el: HTMLTextAreaElement) => void
}) {
  return (
    <textarea
      id={props.id}
      placeholder={props.placeholder}
      value={props.value}
      readOnly={props.readOnly}
      ref={props.ref}
      style={{
        width: '100%',
        height: rows(TICKET_ROWS),
        padding: `0 ${cells(1)}`,
        resize: 'vertical',
        background: t.bgSunken,
        color: t.fg,
        border: 0,
        outline: `var(--ms-border-width) solid ${t.border}`,
        outlineOffset: 0,
        lineHeight: 'var(--ms-row)',
      }}
    />
  )
}
