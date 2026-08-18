/**
 * `/info/<ticket>` — the torrent-style session panel.
 */

import { Button } from 'moonspace-dom'
import { component } from 'visage-dom'
import { useLocation } from 'visage-router'

import { useShareNav } from 'agent-share-ui/nav'
import { SessionChrome } from 'agent-share-ui/SessionChrome'
import { useSession } from 'agent-share-ui/Session/session'
import { TechInfo } from 'agent-share-ui/TechInfo'
import { filesUnder } from 'agent-share-core/tree'
import { parseDev } from 'agent-share-core/ticket'

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
