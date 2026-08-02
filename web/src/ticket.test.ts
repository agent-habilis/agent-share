import { describe, expect, test, beforeEach } from 'bun:test'

import {
  navigateToShare,
  onRouteChange,
  parseRoute,
  parseShareInput,
  sharePath,
  shareUrl,
} from './ticket.ts'

const TICKET = 'testTicketAbc123'

describe('sharePath / shareUrl', () => {
  test('encodes the ticket as one path segment', () => {
    expect(sharePath(TICKET, 'files')).toBe(`/files/${encodeURIComponent(TICKET)}`)
    expect(sharePath(TICKET, 'info')).toBe(`/info/${encodeURIComponent(TICKET)}`)
  })

  test('defaults to the files view', () => {
    expect(sharePath(TICKET)).toBe(`/files/${encodeURIComponent(TICKET)}`)
    expect(shareUrl(TICKET)).toBe(
      `http://localhost/files/${encodeURIComponent(TICKET)}`,
    )
  })

  test('shareUrl is origin-absolute', () => {
    expect(shareUrl(TICKET, 'info')).toBe(
      `http://localhost/info/${encodeURIComponent(TICKET)}`,
    )
  })
})

describe('parseRoute', () => {
  test('reads files and info routes', () => {
    expect(parseRoute(`/files/${encodeURIComponent(TICKET)}`)).toEqual({
      view: 'files',
      ticket: TICKET,
    })
    expect(parseRoute(`/info/${encodeURIComponent(TICKET)}`)).toEqual({
      view: 'info',
      ticket: TICKET,
    })
  })

  test('a bare ticket segment needs no decoding', () => {
    // Tickets are ASCII Base58, so encodeURIComponent is a no-op on them —
    // the encoded and raw forms of a route must parse identically.
    expect(sharePath(TICKET)).toBe(`/files/${TICKET}`)
    expect(parseRoute(`/files/${TICKET}`)).toEqual({
      view: 'files',
      ticket: TICKET,
    })
  })

  test('ignores trailing slashes', () => {
    expect(parseRoute(`/files/${encodeURIComponent(TICKET)}/`)).toEqual({
      view: 'files',
      ticket: TICKET,
    })
  })

  test('returns null for home and unknown paths', () => {
    expect(parseRoute('/')).toBeNull()
    expect(parseRoute('')).toBeNull()
    expect(parseRoute('/about')).toBeNull()
    expect(parseRoute('/files')).toBeNull()
    expect(parseRoute(`/other/${encodeURIComponent(TICKET)}`)).toBeNull()
  })
})

describe('parseShareInput', () => {
  test('accepts a bare ticket', () => {
    expect(parseShareInput(TICKET)).toBe(TICKET)
    expect(parseShareInput(`  ${TICKET}  `)).toBe(TICKET)
  })

  test('accepts path share URLs', () => {
    expect(parseShareInput(`http://localhost/files/${encodeURIComponent(TICKET)}`)).toBe(
      TICKET,
    )
    expect(parseShareInput(`/info/${encodeURIComponent(TICKET)}`)).toBe(TICKET)
  })

  test('returns null for empty or non-share URLs', () => {
    expect(parseShareInput('')).toBeNull()
    expect(parseShareInput('   ')).toBeNull()
    expect(parseShareInput('http://localhost/about')).toBeNull()
    expect(parseShareInput('/about')).toBeNull()
    expect(parseShareInput(`http://localhost/#${encodeURIComponent(TICKET)}`)).toBeNull()
    expect(parseShareInput(`#${TICKET}`)).toBeNull()
  })
})

describe('navigateToShare', () => {
  beforeEach(() => {
    window.history.replaceState(null, '', '/')
  })

  test('pushes a share path and notifies listeners', () => {
    let hits = 0
    const stop = onRouteChange(() => {
      hits += 1
    })
    navigateToShare(TICKET, 'info')
    expect(window.location.pathname).toBe(`/info/${encodeURIComponent(TICKET)}`)
    expect(hits).toBe(1)
    stop()
  })

  test('can replace the current entry', () => {
    navigateToShare(TICKET, 'files')
    navigateToShare(TICKET, 'info', { replace: true })
    expect(window.location.pathname).toBe(`/info/${encodeURIComponent(TICKET)}`)
  })
})
