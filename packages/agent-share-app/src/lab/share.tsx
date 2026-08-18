/**
 * File share: a real `ShareProducer` over real files.
 *
 * The same path the share buttons in the app take, with the picker replaced by
 * something that needs no user gesture — which is what makes it drivable from
 * `tasks/src/e2e.rs`.
 */

import { Button, Input, Select, Stack, Text } from 'moonspace-dom'
import { component, signal } from 'visage-dom'

import { Panel } from 'agent-share-ui/Panel'
import { createShareDirectory, removeShareDirectory, writeOpfsFile } from 'agent-share-core/opfs'
import {
  directorySource,
  snapshotSource,
  startProducer as startShareProducer,
  type ShareProducer,
} from 'agent-share-core/produce'
import { Field, TicketBox, createLog, jsError, type Log } from './parts.tsx'

/** Hoisted for its identity — see `producer.tsx`. */
const SOURCES = [
  { value: 'opfs', label: 'opfs' },
  { value: 'snapshot', label: 'snapshot' },
]

/**
 * A folder full of files, with no user gesture. See `lib/opfs` for why this
 * works and what it costs.
 *
 * The tree deliberately includes a nested directory and a zero-byte file. Both
 * are shapes the manifest treats specially — a directory entry that has to
 * survive with nothing under it, and a file whose slot exists with no bytes to
 * read — and neither appears anywhere in `BenchProducer`'s synthetic stream.
 *
 * @returns the directory handle and the name it lives under, so `stop` can
 * remove it.
 */
async function opfsShareRoot(
  count: number,
  log: Log,
): Promise<{
  handle: FileSystemDirectoryHandle
  name: string
}> {
  // Logged step by step: every one of these can hang or be denied depending on
  // the profile and the storage policy the browser was launched with, and a
  // silent stall here is indistinguishable from a slow producer.
  log('opening the origin private file system…')
  const { handle, name } = await createShareDirectory('lab-share')
  log(`directory ${name} created`)

  await writeOpfsFile(handle, 'blob.bin', 'x'.repeat(64 * 1024))
  log('blob.bin written')
  await writeOpfsFile(handle, 'empty.txt', '')
  await writeOpfsFile(handle, 'nested/deep.txt', 'nested file\n')
  for (let index = 0; index < count; index += 1) {
    await writeOpfsFile(handle, `f${String(index).padStart(3, '0')}.txt`, `file ${index}\n`)
  }
  return { handle, name }
}

/**
 * The same tree as [`opfsShareRoot`], built as `File`s instead of handles.
 *
 * This is what Safari and Firefox produce from `<input type="file">`, and the
 * only way to drive that branch headlessly: the input needs a real click, but
 * a `File` can be constructed, and `webkitRelativePath` — a read-only getter —
 * takes an own property that shadows it.
 */
function snapshotShareFiles(count: number): File[] {
  const at = (relPath: string, contents: string): File => {
    const name = relPath.slice(relPath.lastIndexOf('/') + 1)
    const file = new File([contents], name)
    Object.defineProperty(file, 'webkitRelativePath', { value: `lab-share/${relPath}` })
    return file
  }
  const files = [
    at('blob.bin', 'x'.repeat(64 * 1024)),
    at('empty.txt', ''),
    at('nested/deep.txt', 'nested file\n'),
  ]
  for (let index = 0; index < count; index += 1) {
    files.push(at(`f${String(index).padStart(3, '0')}.txt`, `file ${index}\n`))
  }
  return files
}

export const SharePanel = component(function* () {
  const { view: logView, log } = createLog('share-log')
  const ticket = signal('')
  const running = signal(false)
  /**
   * The share this tab is serving, and the OPFS directory behind it — `null`
   * for a snapshot share, which owns no directory to clean up.
   */
  let share: { producer: ShareProducer; root: string | null } | null = null
  /** The live controls, read at click time — see `parts.tsx`. */
  let filesEl: HTMLInputElement | null = null
  let modeEl: HTMLSelectElement | null = null
  let passwordEl: HTMLInputElement | null = null

  /**
   * Serve a real file share, from OPFS handles or from picked-file snapshots.
   *
   * Everything after the source comes from the app: `startProducer` drives the
   * wasm `ShareProducer` exactly as the share buttons do. Only where the files
   * came from differs, which is the point — a test that built its own producer
   * would prove nothing about the one users get.
   */
  async function start(): Promise<void> {
    if (share) {
      log('already sharing — stop first')
      return
    }
    const count = Number.parseInt(filesEl?.value ?? '', 10) || 1
    const mode = modeEl?.value === 'snapshot' ? 'snapshot' : 'opfs'
    const password = passwordEl?.value ?? ''

    let source
    let root: string | null = null
    if (mode === 'snapshot') {
      log(`building ${count} files as a picked-file snapshot…`)
      source = snapshotSource(snapshotShareFiles(count))
      log('built')
    } else {
      log(`seeding ${count} files into the origin private file system…`)
      const seeded = await opfsShareRoot(count, log)
      root = seeded.name
      source = directorySource(seeded.handle)
      log('seeded')
    }
    log(`ShareProducer.start(${password ? 'with password' : 'no password'})…`)
    const started = await startShareProducer(source, password || undefined)
    share = { producer: started, root }
    ticket.value = started.ticket
    running.value = true
    log(
      `file share ready — ${started.files} files, ${started.bytes} bytes`,
      started.passwordProtected ? '(password required)' : '',
    )
  }

  /** Stop the share and remove the OPFS directory, if it had one. */
  async function stop(): Promise<void> {
    if (!share) {
      log('not sharing')
      return
    }
    log('stopping…')
    const current = share
    share = null
    running.value = false
    await current.producer.stop()
    if (current.root !== null && !(await removeShareDirectory(current.root))) {
      log('could not remove the OPFS directory')
    }
    log('stopped')
  }

  yield () => (
    <Panel title="File share">
      <Stack direction="column" gap={1}>
        <Text color="fgMuted">
          A real ShareProducer over real files. <b>opfs</b> serves genuine
          FileSystemFileHandles from the origin private file system, the live path Chromium
          takes. <b>snapshot</b> serves constructed Files, the pinned path Safari and Firefox
          take from a file input.
        </Text>
        <Stack direction="row" gap={2} align="center" wrap>
          <Field label="files">
            <Input
              id="share-files"
              type="number"
              min="1"
              max="200"
              value="3"
              width={6}
              ref={(el) => {
                filesEl = el
              }}
            />
          </Field>
          <Field label="source">
            <Select
              id="share-mode"
              width={12}
              options={SOURCES}
              ref={(el) => {
                modeEl = el
              }}
            />
          </Field>
          <Field label="password">
            <Input
              id="share-password"
              type="password"
              placeholder="(none)"
              width={14}
              ref={(el) => {
                passwordEl = el
              }}
            />
          </Field>
        </Stack>
        <Stack direction="row" gap={2} align="center" wrap>
          <Button
            id="share-start"
            variant="primary"
            onclick={() => {
              void start().catch((error: unknown) => log('FAILED', jsError(error)))
            }}
          >
            Start
          </Button>
          <Button
            id="share-stop"
            variant="secondary"
            disabled={!running.value}
            onclick={() => {
              void stop().catch((error: unknown) => log('FAILED', jsError(error)))
            }}
          >
            Stop
          </Button>
          <Button
            id="share-copy"
            variant="ghost"
            disabled={ticket.value === ''}
            onclick={() => {
              void navigator.clipboard.writeText(ticket.peek())
              log('ticket copied')
            }}
          >
            Copy ticket
          </Button>
        </Stack>
        <TicketBox
          id="share-ticket"
          placeholder="ticket appears here"
          value={ticket.value}
          readOnly
        />
        {logView}
      </Stack>
    </Panel>
  )
})
