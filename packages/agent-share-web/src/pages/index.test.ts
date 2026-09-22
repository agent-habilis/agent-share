import { beforeEach, describe, expect, test } from 'bun:test'
import { component, flushSync, render, tags } from 'visage-dom'
import { Outlet, createRouter, flattenRoutes, matchRoutes, memoryHistory } from 'visage-router'
import type { RouteDef } from 'visage-router'

import { ROUTES } from './index.ts'
import { FilesPage } from './files/index.tsx'
import { HomePage } from './home/index.tsx'
import { InfoPage } from './info/index.tsx'
import { PreviewPage } from './preview/index.tsx'
import { SessionLayout } from '../components/session/index.tsx'
import { clientKey } from '../lib/client/index.ts'

const TICKET = 'testTicketAbc123'

/**
 * Which components a URL resolves to, root first.
 *
 * Matching rather than rendering, because mounting `SessionLayout` for real
 * would dial a share: it is the route table under test here, not the session.
 */
const branches = flattenRoutes(ROUTES as readonly RouteDef[])
function resolve(pathname: string): unknown[] {
  return (matchRoutes(branches, pathname) ?? []).map((match) => match.route.component)
}

describe('the route table', () => {
  test('each share view resolves to its own page under one layout', () => {
    expect(resolve(`/files/${TICKET}`)).toEqual([SessionLayout, FilesPage])
    expect(resolve(`/info/${TICKET}`)).toEqual([SessionLayout, InfoPage])
    expect(resolve(`/preview/${TICKET}/docs/note.md`)).toEqual([SessionLayout, PreviewPage])
  })

  test('the bare root is home, not the session layout', () => {
    // Two routes are declared at `/`; only the childless one may take it.
    expect(resolve('/')).toEqual([HomePage])
  })

  test('a preview naming no file is still a preview route', () => {
    // Reached by hand it has nothing to show, and the pane says so — falling
    // back to home would send the tab to the landing page instead.
    expect(resolve(`/preview/${TICKET}`)).toEqual([SessionLayout, PreviewPage])
  })

  test('nothing matches what the fallback has to catch', () => {
    // Each of these rendered `Home` before the router, and the `fallback:
    // HomePage` in the table is what keeps that true.
    expect(matchRoutes(branches, '/garbage')).toBeNull()
    expect(matchRoutes(branches, '/files')).toBeNull()
    expect(matchRoutes(branches, '/files/')).toBeNull()
    expect(matchRoutes(branches, `/files/${TICKET}/extra`)).toBeNull()
  })

  test('the ticket reaches the layout as a param', () => {
    const matched = matchRoutes(branches, `/info/${TICKET}`)
    expect(matched?.[0]?.params['ticket']).toBe(TICKET)
  })

  test('switching views keeps the same layout route object', () => {
    // What makes `/files` ↔ `/info` free: the router keeps a depth-0 route
    // component mounted while its route object is unchanged.
    const files = matchRoutes(branches, `/files/${TICKET}`)
    const info = matchRoutes(branches, `/info/${TICKET}`)
    expect(files?.[0]?.route).toBe(info?.[0]?.route!)
  })

  test('a different share is a different session key', () => {
    // The layout route object is stable across a *ticket* change too, which is
    // why `SessionLayout` keys the session it mounts rather than trusting the
    // router to remount it.
    expect(clientKey('a', undefined)).not.toBe(clientKey('b', undefined))
    expect(clientKey('a', 'webrtc')).not.toBe(clientKey('a', 'dynamic'))
  })
})

/*
  The same shape as `ROUTES` — two routes at `/`, one of them a layout with the
  three share views under it — with the real components swapped for stubs, so
  the arrangement can be mounted without dialling anything.
*/
describe('the layout arrangement', () => {
  const { div, span } = tags
  let host: HTMLElement

  beforeEach(() => {
    document.body.innerHTML = ''
    host = document.createElement('div')
    document.body.appendChild(host)
  })

  let layoutMounts = 0
  const Layout = component(function* () {
    layoutMounts += 1
    yield () => div('session[', Outlet(), ']')
  })
  const stub = (name: string) =>
    component(function* () {
      yield () => span(name)
    })

  const SHAPE: RouteDef[] = [
    { path: '/', component: stub('home') },
    {
      path: '/',
      component: Layout,
      children: [
        { path: 'files/:ticket', component: stub('files') },
        { path: 'info/:ticket', component: stub('info') },
        { path: 'preview/:ticket/*', component: stub('preview') },
      ],
    },
  ]

  test('a view switch swaps the page and keeps the session', () => {
    layoutMounts = 0
    const history = memoryHistory([`/files/${TICKET}`])
    const App = createRouter({ routes: SHAPE, fallback: stub('home'), scroll: false, history })
    const root = render(App(), host)
    flushSync()
    expect(host.textContent).toBe('session[files]')
    expect(layoutMounts).toBe(1)

    history.push(`/info/${TICKET}`)
    flushSync()
    expect(host.textContent).toBe('session[info]')
    // The whole point: no second mount, so no redial.
    expect(layoutMounts).toBe(1)
    root.unmount()
  })

  test('home renders outside the session layout', () => {
    layoutMounts = 0
    const App = createRouter({
      routes: SHAPE,
      fallback: stub('home'),
      scroll: false,
      history: memoryHistory(['/']),
    })
    const root = render(App(), host)
    flushSync()
    expect(host.textContent).toBe('home')
    expect(layoutMounts).toBe(0)
    root.unmount()
  })

  test('an unmatched path falls back to home', () => {
    const App = createRouter({
      routes: SHAPE,
      fallback: stub('home'),
      scroll: false,
      history: memoryHistory(['/garbage']),
    })
    const root = render(App(), host)
    flushSync()
    expect(host.textContent).toBe('home')
    root.unmount()
  })
})
