import { describe, expect, test, beforeEach } from 'bun:test'

import { canGoBack, shareTarget } from './nav.ts'

const TICKET = 'testTicketAbc123'

beforeEach(() => {
  window.history.replaceState(null, '', '/')
})

describe('shareTarget', () => {
  test('carries the current mode across views', () => {
    expect(shareTarget('?transport=webrtc', TICKET, 'info')).toBe(
      `/info/${TICKET}?transport=webrtc`,
    )
  })

  test('carries the dev flag, which lives in the pane it leads to', () => {
    expect(shareTarget('?dev=true', TICKET, 'info')).toBe(`/info/${TICKET}?dev=true`)
  })

  test('adds nothing when nothing is pinned', () => {
    expect(shareTarget('', TICKET, 'info')).toBe(`/info/${TICKET}`)
  })

  test('writes a preview path under the ticket', () => {
    expect(shareTarget('?transport=relay', TICKET, 'preview', ['docs', 'note.md'])).toBe(
      `/preview/${TICKET}/docs/note.md?transport=relay`,
    )
  })

  test('defaults to the files view', () => {
    expect(shareTarget('', TICKET)).toBe(`/files/${TICKET}`)
  })
})

describe('canGoBack', () => {
  test('a fresh document has nothing behind it', () => {
    // What a pasted link or a new tab lands on: `back()` here leaves the site.
    window.history.replaceState(null, '', `/preview/${TICKET}/note.txt`)
    expect(canGoBack()).toBe(false)
  })

  test('the router entry key alone is not a push of ours', () => {
    // `browserHistory` stamps every entry — including a cold load's — with its
    // own key. Only the nested user state says this tab pushed the entry.
    window.history.replaceState({ key: 'abc12345' }, '', `/files/${TICKET}`)
    expect(canGoBack()).toBe(false)
  })

  test('an entry this tab pushed can be popped', () => {
    window.history.replaceState({ key: 'abc12345', usr: { agentShare: 1 } }, '', '/x')
    expect(canGoBack()).toBe(true)
  })

  test('reads whatever state it is handed', () => {
    expect(canGoBack({ agentShare: 1 })).toBe(true)
    expect(canGoBack(undefined)).toBe(false)
    expect(canGoBack(null)).toBe(false)
    expect(canGoBack('agentShare')).toBe(false)
  })
})
