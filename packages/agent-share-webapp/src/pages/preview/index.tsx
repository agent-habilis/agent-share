/**
 * `/preview/<ticket>/<path…>` — one file, rendered in place.
 *
 * The URL names the file, not the session's selection signal, so a preview
 * survives a reload and a pasted link opens the same one.
 */

import { Button } from 'moonspace-dom'
import { component } from 'visage-dom'
import { useLocation } from 'visage-router'

import { canGoBack, useShareNav } from '../../components/nav.ts'
import { Preview } from '../../components/preview/index.tsx'
import { SessionChrome } from '../../components/session-chrome/index.tsx'
import { useSession } from '../../components/session/session.ts'
import { keepsSettled } from '../../lib/keep/index.ts'
import { previewSegments } from '../../lib/ticket/index.ts'
import { nodeAtPath } from '../../lib/tree.ts'

export const PreviewPage = component(function* () {
  const session = useSession(this)
  const nav = useShareNav(this)
  const location = useLocation(this)

  /*
    A preview pulls the whole file through the same reader a download does, so
    leaving one is the moment those chunks become servable. Abandoning it
    part-way still leaves the chunks that landed, and those count.

    Hung off unmount rather than off `close`, because `close` is only half the
    exits: the browser's Back button is a `popstate`, which swaps this
    component out without ever calling it — and a preview left that way used to
    hold the whole file and advertise none of it.

    On the page rather than on `Preview`, which remounts per file: publishing
    recomputes what this tab serves across the whole store, so once on the way
    out covers every file looked at on the way in.

    Waiting on the keeps is what stops the publish from reading a store the
    last chunks have not reached yet — they are stored beside the transfer, not
    inside it.
  */
  this.aborted.addEventListener('abort', () => {
    void keepsSettled().then(() => session.publishHoldings())
  })

  /*
    Cancelling a preview goes *back*, rather than pushing the files view on
    top of it. Pushing left the preview sitting one Back press away from a
    user who had just asked to leave it — and every open-then-cancel added
    two more entries to walk through.

    A pasted link has nothing behind it, so that case replaces instead, and
    lands the column browser on the file that was on screen — otherwise the
    only way out of a preview link is the root of the share.
  */
  const close = () => {
    if (canGoBack()) {
      nav.back()
      return
    }
    const named = previewSegments(location.peek().pathname)
    if (named.length > 0) session.path.value = named
    nav.go(session.ticket, 'files', { replace: true })
  }

  yield () => {
    const ready = session.ready.value
    if (!ready) return null
    const named = previewSegments(location.value.pathname)
    const node = nodeAtPath(ready.root, named)
    const file = node?.kind === 'file' ? node : undefined

    return (
      <SessionChrome
        session={session}
        crumb="preview"
        trailing={
          /*
            One button, and it leaves. The transfer branch the files page
            shows ends in a `Cancel` that aborts a download, and two buttons
            called Cancel — one leaving the view, one stopping a transfer — is
            a coin flip the user has to lose once to learn.

            Downloading lives in the detail pane that `Cancel` returns to, on
            the same file, so a second copy of it here would be a second
            answer to a question already answered one screen away.
          */
          <Button variant="secondary" onclick={close}>
            Cancel
          </Button>
        }
      >
        {/*
          Keyed by the file: switching previews starts a fresh load rather
          than racing the old one, and unmounting is what aborts it.
        */}
        <Preview key={file?.path ?? ''} node={file} client={ready.client} onClose={close} />
      </SessionChrome>
    )
  }
})
