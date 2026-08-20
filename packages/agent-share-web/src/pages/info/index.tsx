/**
 * `/info/<ticket>` — the torrent-style session panel.
 */

import { Button } from 'moonspace-dom'
import { component } from 'visage-dom'
import { useLocation } from 'visage-router'

import { useShareNav } from '../../components/nav.ts'
import { SessionChrome } from '../../components/session-chrome/index.tsx'
import { useSession } from '../../components/session/session.ts'
import { TechInfo } from '../../components/tech-info/index.tsx'
import { filesUnder } from '../../lib/tree.ts'
import { parseDev } from '../../lib/ticket/index.ts'

export const InfoPage = component(function* () {
  const session = useSession(this)
  const nav = useShareNav(this)
  const location = useLocation(this)

  // The peer-IP refresh is the session sampler's, not this page's — there is
  // exactly one sampler, and a second would zero the first's rates. This asks
  // it for the fast cadence for as long as the pane is on screen.
  session.wantsPeerIps.value = true
  this.aborted.addEventListener('abort', () => {
    session.wantsPeerIps.value = false
  })

  const close = () => nav.go(session.ticket, 'files')

  yield () => {
    const ready = session.ready.value
    if (!ready) return null
    const files = filesUnder(ready.root)

    return (
      <SessionChrome
        session={session}
        crumb="info"
        trailing={
          <Button variant="ghost" onclick={close}>
            Close
          </Button>
        }
      >
        <TechInfo
          client={ready.client}
          tick={session.tick}
          files={files}
          held={session.held}
          coverage={session.coverage}
          history={session.history}
          openedAt={session.openedAt}
          lastActivityAt={session.lastActivityAt}
          status={session.status.value}
          mounted={session.mounted.value}
          mountError={session.mountError.value}
          dev={parseDev(location.value.search)}
          killDisabled={session.redialling.value}
          onKillConnection={() => {
            ready.client.close_connection()
          }}
          onClose={close}
        />
      </SessionChrome>
    )
  }
})
