/**
 * `/files/<ticket>` — the column browser, and the actions that act on it.
 */

import { Button, ProgressBar, Stack } from 'moonspace-dom'
import { component } from 'visage-dom'
import type { Child } from 'visage-dom'

import { useShareNav } from '../../components/nav.ts'
import { ColumnView } from '../../components/column-view/index.tsx'
import { SessionChrome } from '../../components/session-chrome/index.tsx'
import { transferLabel, useSession } from '../../components/session/session.ts'
import { canMount } from '../../lib/mount/index.ts'
import { seedState } from '../../lib/seeding/index.ts'

export const FilesPage = component(function* () {
  const session = useSession(this)
  const nav = useShareNav(this)

  yield () => {
    const ready = session.ready.value
    if (!ready) return null

    const active = session.transfer.value
    const mounted = session.mounted.value
    // Re-dialling a connection lost to a background tab. Browsing is unaffected
    // — the tree is local — so only the actions that reach the peer wait.
    const redialling = session.redialling.value
    const mountable = canMount() && !redialling

    const mountButton = () => (
      <Button variant="ghost" onclick={session.mount} disabled={!mountable}>
        {mounted ? 'Unmount' : 'Mount'}
      </Button>
    )
    /*
      Whole-share seeding. The label carries the state rather than a separate
      line, because this row is exactly `oneRow` tall and anything taller
      would move every pixel of content under it.

      Pressable exactly when pressing it would do something — so the label is
      that same predicate rather than a state of its own. Counts stay out of
      it: once everything is held there is nothing to act on, and the number
      of files still missing is already in the detail column.
    */
    const idle =
      !session.seeding.value &&
      seedState(ready.root, session.held.value, session.coverage.value) !== 'full'

    /*
      One row, always. A transfer takes the middle of the row rather than
      adding one below it — every child here is exactly `oneRow` tall, so
      the content underneath never moves.
    */
    let trailing: Child
    if (active) {
      trailing = (
        <>
          <ProgressBar
            fluid
            showValue
            value={
              active.progress.total === 0 ? 0 : active.progress.done / active.progress.total
            }
            label={transferLabel(active.kind)}
          />
          <Button variant="danger" onclick={() => active.abort.abort()}>
            Cancel
          </Button>
        </>
      )
    } else {
      trailing = (
        <Stack direction="row" gap={1}>
          <Button variant="ghost" onclick={session.seedShare} disabled={!idle}>
            {idle ? 'Seed' : 'Seeding'}
          </Button>
          <Button variant="ghost" onclick={() => nav.go(session.ticket, 'info')}>
            Info
          </Button>
          {mountable ? (
            mountButton()
          ) : (
            /*
              The `title` goes on a wrapper, not on the button: a disabled
              control is an unreliable tooltip host, since browsers suppress
              pointer delivery to it. `inline-flex` keeps the wrapper exactly
              `oneRow` tall — a default `inline` span adds line-box leading
              and would break the invariant this row is built on.
            */
            <span
              title="Mounting needs the File System Access API, which this browser lacks. Use Chrome or Edge — or run `npx agent-share <ticket>` to receive the folder locally."
              style={{ display: 'inline-flex' }}
            >
              {mountButton()}
            </span>
          )}
          <Button variant="primary" onclick={session.downloadAll} disabled={redialling}>
            Download
          </Button>
        </Stack>
      )
    }

    return (
      <SessionChrome
        session={session}
        // Never "reconnecting": redialing is the app's permanent background
        // posture — it is always willing to reach more peers — so naming it in
        // the chrome would label the normal state of the world. A transfer
        // whose peer is being re-dialed simply shows its own label until bytes
        // resume.
        crumb={active ? transferLabel(active.kind) : 'files'}
        trailing={trailing}
      >
        <ColumnView
          root={ready.root}
          path={session.path.value}
          onPathChange={session.onPathChange}
          onDownload={session.onDownload}
          downloadDisabled={active !== null || redialling}
          held={session.held.value}
          coverage={session.coverage.value}
          onSeed={session.onSeed}
          seedDisabled={session.seeding.value || redialling}
          onPreview={session.onPreview}
          previewDisabled={active !== null || redialling}
        />
      </SessionChrome>
    )
  }
})
