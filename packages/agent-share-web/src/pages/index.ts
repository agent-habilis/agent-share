/**
 * The route table. One folder under `pages/` per share view. Paths are
 * relative to `APP_BASE` (`/app`): the router strips and adds it, so
 * `/files/<ticket>` here is `/app/files/<ticket>` in the address bar.
 *
 * - `/` — home
 * - `/files/<ticket>` — file browser
 * - `/info/<ticket>` — session info
 * - `/preview/<ticket>/<path…>` — one file, rendered in place
 *
 * The three share views hang off one pathless layout so the session under them
 * is mounted once: switching views swaps the page and keeps the client, the
 * mesh membership and the sampler. `SessionLayout` re-keys on the ticket, which
 * is what still forces a redial when the *share* changes.
 *
 * Anything unmatched falls back to home, which is what the hand-rolled parser
 * this replaced did for `/garbage`, `/files/` and `/files/a/b` alike.
 */

import { browserHistory, createRouter, type RouteDef } from 'visage-router'

import { SessionLayout } from '../components/session/index.tsx'
import { APP_BASE } from '../lib/ticket/index.ts'
import { FilesPage } from './files/index.tsx'
import { HomePage } from './home/index.tsx'
import { InfoPage } from './info/index.tsx'
import { PreviewPage } from './preview/index.tsx'

export const ROUTES: readonly RouteDef[] = [
  { path: '/', component: HomePage },
  {
    path: '/',
    component: SessionLayout,
    children: [
      { path: 'files/:ticket', component: FilesPage },
      { path: 'info/:ticket', component: InfoPage },
      { path: 'preview/:ticket/*', component: PreviewPage },
    ],
  },
]

/**
 * `scroll: false` deliberately. The app is a fixed `100vh` flex column that
 * scrolls inside its own panes, so there is no page scroll to restore — and
 * the router's scroll manager would add a `scroll` listener, take over
 * `history.scrollRestoration` and `flushSync()` on every navigation to pay
 * for it.
 */
export const App = createRouter({
  routes: ROUTES,
  history: browserHistory({ base: APP_BASE }),
  fallback: HomePage,
  scroll: false,
})
