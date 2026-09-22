/**
 * The landing page: create a share from a local folder, or join one by ticket.
 */

import { Badge, Box, Button, Input, Stack, Text } from 'moonspace-dom'
import { component, signal } from 'visage-dom'

import { Centered } from '../../components/centered/index.tsx'
import { Chrome } from '../../components/chrome/index.tsx'
import { copyText } from '../../lib/clipboard/index.ts'
import { FailedBody } from '../../components/failed-body/index.tsx'
import { LoadingBody } from '../../components/loading-body/index.tsx'
import { useShareNav } from '../../components/nav.ts'
import { pickShareFiles, type PickMode } from '../../lib/pick-share-files/index.ts'
import {
  canProduceLive,
  directorySource,
  pickShareRoot,
  snapshotSource,
  startProducer,
  type ShareProducer,
  type ShareSource,
} from '../../lib/produce.ts'
import { parseShareInput, shareUrl } from '../../lib/ticket/index.ts'
import { humanBytes } from '../../lib/tree.ts'

type HomeState =
  | { phase: 'landing' }
  | { phase: 'creating' }
  | { phase: 'serving'; producer: ShareProducer }
  | { phase: 'failed'; reason: string }

export const HomePage = component(function* (_props) {
  // Nested plain functions below capture `ctx`; `this` would not reach them.
  const ctx = this
  const nav = useShareNav(this)
  const state = signal<HomeState>({ phase: 'landing' })
  /**
   * Optional password for the share about to be created. Read at the moment
   * the folder is picked, not stored: this is the only place it exists, and
   * once `ShareProducer` has stretched it into a token nothing needs it again.
   */
  let newSharePassword = ''

  /**
   * Open a picker and serve what comes back.
   *
   * The picker call is the first thing that happens, before any `await`. Both
   * `showDirectoryPicker()` and `input.click()` spend the transient user
   * activation from the click that got us here, and an await hoisted above
   * either one breaks the pick — in Safari silently, with a `NotAllowedError`
   * no Chromium test run would ever see.
   */
  async function createShare(mode: PickMode): Promise<void> {
    const pick: Promise<ShareSource> =
      mode === 'folder' && canProduceLive()
        ? pickShareRoot().then(directorySource)
        : pickShareFiles(mode).then(snapshotSource)
    try {
      const source = await pick
      if (ctx.aborted.aborted) return
      state.value = { phase: 'creating' }
      // Empty means unprotected. An empty string is not a password, and
      // passing one would protect the share with something nobody can type.
      const producer = await startProducer(
        source,
        newSharePassword.length > 0 ? newSharePassword : undefined,
      )
      if (ctx.aborted.aborted) {
        await producer.stop()
        return
      }
      state.value = { phase: 'serving', producer }
    } catch (error) {
      if (error instanceof DOMException && error.name === 'AbortError') return
      if (!ctx.aborted.aborted) {
        state.value = { phase: 'failed', reason: String(error) }
      }
    }
  }

  function joinShare(): void {
    const raw = window.prompt('Paste a share ticket or URL')
    if (raw === null) return
    const ticket = parseShareInput(raw)
    if (!ticket) return
    nav.go(ticket, 'files')
  }

  async function stopServing(): Promise<void> {
    const current = state.peek()
    if (current.phase !== 'serving') return
    // Leave 'serving' before awaiting, not after. The teardown takes a mesh
    // departure broadcast and an endpoint close; while that runs the button is
    // still on screen, and a second click used to re-read 'serving' and call
    // `stop()` again.
    state.value = { phase: 'landing' }
    try {
      await current.producer.stop()
    } catch (error) {
      // Nothing left to recover: the share is already off the UI. Report it
      // rather than surfacing an unhandled rejection.
      console.warn('[share] stopping the share failed', error)
    }
  }

  ctx.aborted.addEventListener('abort', () => {
    const current = state.peek()
    if (current.phase === 'serving') void current.producer.stop()
  })

  yield () => {
    const current = state.value
    if (current.phase === 'creating') {
      return (
        <Chrome>
          <LoadingBody label="creating share…" />
        </Chrome>
      )
    }
    if (current.phase === 'failed') {
      return (
        <Chrome
          trailing={
            <Button variant="secondary" onclick={() => {
              state.value = { phase: 'landing' }
            }}>
              Back
            </Button>
          }
        >
          <FailedBody reason={current.reason} />
        </Chrome>
      )
    }
    if (current.phase === 'serving') {
      const url = shareUrl(current.producer.ticket)
      return (
        <Chrome
          trailing={
            <Button variant="danger" onclick={() => void stopServing()}>
              Stop sharing
            </Button>
          }
        >
          <Centered>
            <div style={{ padding: '0 2ch', maxWidth: '72ch', width: '100%' }}>
              <Box border="line" padX={2} padY={1}>
                <Stack direction="column" gap={1}>
                  <Stack direction="row" gap={1}>
                    <Text weight="bold">Sharing</Text>
                    <Badge tone="success" variant="outline">
                      {current.producer.transport}
                    </Badge>
                    <Text color="fgMuted">
                      {current.producer.files} files · {humanBytes(current.producer.bytes)}
                    </Text>
                  </Stack>
                  <Text color="fgMuted">Peers open this link:</Text>
                  {/*
                    `Copy link` below takes the whole string; selection is for
                    taking part of it — the ticket alone, say.
                  */}
                  <Text class="selectable">{url}</Text>
                  {/*
                    The password is deliberately not in the link — that is what
                    makes the link postable. Say so, so the sender knows the
                    recipient will be asked for something they have to supply.
                  */}
                  {current.producer.passwordProtected ? (
                    <Text color="fgMuted">
                      Password-protected — send the password separately; the link alone
                      will not open it.
                    </Text>
                  ) : null}
                  {/*
                    A snapshot share cannot be rescanned, so say it here rather
                    than let the sender discover it as a read error on the far
                    end after they edit a file.
                  */}
                  {current.producer.live ? null : (
                    <Text color="fgMuted">
                      Snapshot — these files as they were when you picked them. Edits
                      will not reach peers, and empty folders were not included. Stop
                      and pick again to publish changes.
                    </Text>
                  )}
                  {current.producer.skipped + current.producer.renamed > 0 ? (
                    <Text color="warning">
                      {current.producer.skipped > 0
                        ? `${current.producer.skipped} file(s) left out: unsafe path. `
                        : ''}
                      {current.producer.renamed > 0
                        ? `${current.producer.renamed} renamed to avoid a clash.`
                        : ''}
                    </Text>
                  ) : null}
                  <Button
                    variant="primary"
                    onclick={() => {
                      void copyText(url)
                    }}
                  >
                    Copy link
                  </Button>
                </Stack>
              </Box>
            </div>
          </Centered>
        </Chrome>
      )
    }

    return (
      <Chrome>
        <Centered>
          <Stack direction="column" gap={1}>
            <Button variant="primary" onclick={() => void createShare('folder')}>
              Share a folder
            </Button>
            {/*
              Both buttons everywhere. Loose files are not a Safari consolation
              prize — the directory picker cannot share a hand-picked set at
              all — and on iOS, where no browser offers folder selection, this
              is the only way in.
            */}
            <Button variant="secondary" onclick={() => void createShare('files')}>
              Share files
            </Button>
            {/*
              Above the picker rather than after it: the folder picker needs a
              user gesture, so the password has to already be typed when the
              button is clicked. Empty is the default and means an open share,
              which is what this tool did before passwords existed.
            */}
            <Input
              type="password"
              placeholder="Password (optional)"
              aria-label="Password for the share"
              oninput={(event: Event) => {
                newSharePassword = (event.target as HTMLInputElement).value
              }}
            />
            <Button variant="secondary" onclick={() => joinShare()}>
              Join a share
            </Button>
          </Stack>
        </Centered>
      </Chrome>
    )
  }
})
