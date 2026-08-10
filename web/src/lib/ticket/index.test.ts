import { describe, expect, test, beforeEach } from 'bun:test'

import {
  parseRoute,
  parseShareInput,
  parseTransport,
  previewSegments,
  sharePath,
  shareUrl,
} from './index.ts'

const TICKET = 'testTicketAbc123'

// `parseRoute` and `previewSegments` both default to reading `window.location`,
// so a test that sets the URL would otherwise leak into every later test.
beforeEach(() => {
  window.history.replaceState(null, '', '/')
})

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
      path: [],
    })
    expect(parseRoute(`/info/${encodeURIComponent(TICKET)}`)).toEqual({
      view: 'info',
      ticket: TICKET,
      path: [],
    })
  })

  test('a bare ticket segment needs no decoding', () => {
    // Tickets are ASCII Base58, so encodeURIComponent is a no-op on them —
    // the encoded and raw forms of a route must parse identically.
    expect(sharePath(TICKET)).toBe(`/files/${TICKET}`)
    expect(parseRoute(`/files/${TICKET}`)).toEqual({
      view: 'files',
      ticket: TICKET,
      path: [],
    })
  })

  test('ignores trailing slashes', () => {
    expect(parseRoute(`/files/${encodeURIComponent(TICKET)}/`)).toEqual({
      view: 'files',
      ticket: TICKET,
      path: [],
    })
  })

  test('returns null for home and unknown paths', () => {
    expect(parseRoute('/')).toBeNull()
    expect(parseRoute('')).toBeNull()
    expect(parseRoute('/about')).toBeNull()
    expect(parseRoute('/files')).toBeNull()
    expect(parseRoute(`/other/${encodeURIComponent(TICKET)}`)).toBeNull()
  })

  test('only preview takes segments past the ticket', () => {
    expect(parseRoute(`/files/${TICKET}/extra`)).toBeNull()
    expect(parseRoute(`/info/${TICKET}/extra`)).toBeNull()
  })
})

describe('the preview route', () => {
  test('reads the file path out of the trailing segments', () => {
    expect(parseRoute(`/preview/${TICKET}/docs/note.md`)).toEqual({
      view: 'preview',
      ticket: TICKET,
      path: ['docs', 'note.md'],
    })
  })

  test('a preview naming no file is still a preview route', () => {
    // A `/preview/<ticket>` reached by hand has nothing to show, and the pane
    // says so — parsing it as home would send the tab to the landing page.
    expect(parseRoute(`/preview/${TICKET}`)).toEqual({
      view: 'preview',
      ticket: TICKET,
      path: [],
    })
  })

  test('segments survive characters a URL path cannot carry raw', () => {
    const name = 'a b#c%d?e.txt'
    const url = new URL(
      sharePath(TICKET, 'preview', undefined, false, ['sub dir', name]),
      'http://localhost',
    )
    expect(url.pathname).not.toContain('#')
    expect(parseRoute(url.pathname, url.search)).toEqual({
      view: 'preview',
      ticket: TICKET,
      path: ['sub dir', name],
    })
  })

  test('a preview URL still yields its ticket when pasted', () => {
    expect(parseShareInput(`http://localhost/preview/${TICKET}/docs/note.md`)).toBe(TICKET)
  })

  test('previewSegments reads the file off a raw pathname', () => {
    expect(previewSegments(`/preview/${TICKET}/docs/note.md`)).toEqual(['docs', 'note.md'])
    expect(previewSegments(`/preview/${TICKET}`)).toEqual([])
    expect(previewSegments(`/preview/${TICKET}/`)).toEqual([])
  })

  test('previewSegments splits before decoding', () => {
    // The router hands out params already decoded, which is why this reads the
    // pathname instead: a segment holding an encoded slash must stay one
    // segment, and splitting after decoding would make it two.
    const name = 'a/b.txt'
    const url = new URL(
      sharePath(TICKET, 'preview', undefined, false, [name]),
      'http://localhost',
    )
    expect(previewSegments(url.pathname)).toEqual([name])
  })
})

describe('parseTransport', () => {
  test('accepts the three canonical modes', () => {
    expect(parseTransport('?transport=webrtc')).toBe('webrtc')
    expect(parseTransport('?transport=relay')).toBe('relay')
    expect(parseTransport('?transport=dynamic')).toBe('dynamic')
  })

  test('is case- and whitespace-insensitive', () => {
    expect(parseTransport('?transport=WebRTC')).toBe('webrtc')
    expect(parseTransport('?transport=%20relay%20')).toBe('relay')
  })

  test('treats unknown, empty and absent values as the default', () => {
    // A typo must degrade to the default, not fail the page.
    expect(parseTransport('?transport=turn')).toBeUndefined()
    // The wasm-side aliases are deliberately not part of the URL surface.
    expect(parseTransport('?transport=webrtc_only')).toBeUndefined()
    expect(parseTransport('?transport=')).toBeUndefined()
    expect(parseTransport('?other=webrtc')).toBeUndefined()
    expect(parseTransport('')).toBeUndefined()
  })
})

describe('transport on the route', () => {
  test('parseRoute reads ?transport=', () => {
    expect(parseRoute(`/files/${TICKET}`, '?transport=webrtc')).toEqual({
      view: 'files',
      ticket: TICKET,
      path: [],
      transport: 'webrtc',
    })
  })

  test('a bare route carries no transport key', () => {
    expect(parseRoute(`/files/${TICKET}`, '')).not.toHaveProperty('transport')
  })

  test('sharePath round-trips the mode', () => {
    expect(sharePath(TICKET, 'info', 'webrtc')).toBe(`/info/${TICKET}?transport=webrtc`)
    const url = new URL(sharePath(TICKET, 'info', 'relay'), 'http://localhost')
    expect(parseRoute(url.pathname, url.search)).toEqual({
      view: 'info',
      ticket: TICKET,
      path: [],
      transport: 'relay',
    })
  })

  test('shareUrl stays clean — the pin is local, not part of the capability', () => {
    window.history.replaceState(null, '', `/files/${TICKET}?transport=webrtc`)
    expect(shareUrl(TICKET)).toBe(`http://localhost/files/${TICKET}`)
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
