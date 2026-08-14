/**
 * Lab: synthetic OP_BENCH producer + consumer (transport set by producer).
 */

// First, before anything can build a Disposable. See the file for why.
import '../compat.ts'

import { createShareDirectory, removeShareDirectory, writeOpfsFile } from '../lib/opfs/index.ts'
import { buildPeerCard } from '../lib/peerCard/index.ts'
import {
  directorySource,
  snapshotSource,
  startProducer as startShareProducer,
  type ShareProducer,
} from '../lib/produce.ts'
import { parseShareInput } from '../lib/ticket/index.ts'
import { loadWasm, type WasmModule } from '../wasm/index.ts'

type BenchProducer = Awaited<ReturnType<WasmModule['BenchProducer']['start']>>

let producer: BenchProducer | null = null

/**
 * The file share this tab is serving, and the OPFS directory behind it — `null`
 * for a snapshot share, which owns no directory to clean up.
 */
let share: { producer: ShareProducer; root: string | null; added: number } | null = null

function el<T extends HTMLElement>(id: string): T {
  const node = document.getElementById(id)
  if (!node) throw new Error(`#${id} missing`)
  return node as T
}

function logger(pre: HTMLPreElement) {
  return (...parts: unknown[]) => {
    const line = parts
      .map((part) => {
        if (part instanceof Error) return part.stack || part.message
        if (typeof part === 'string') return part
        try {
          return JSON.stringify(part)
        } catch {
          return String(part)
        }
      })
      .join(' ')
    const stamp = new Date().toISOString().slice(11, 23)
    pre.textContent += `[${stamp}] ${line}\n`
    pre.scrollTop = pre.scrollHeight
    console.log('[lab]', ...parts)
  }
}

function jsError(error: unknown): string {
  if (error instanceof Error) return error.message
  if (typeof error === 'string') return error
  try {
    return JSON.stringify(error)
  } catch {
    return String(error)
  }
}

async function startProducer(
  transport: string,
  log: (...parts: unknown[]) => void,
  ticketBox: HTMLTextAreaElement,
  stopBtn: HTMLButtonElement,
  copyBtn: HTMLButtonElement,
): Promise<void> {
  if (producer) {
    log('already producing — stop first')
    return
  }
  log('loading wasm…')
  const wasm = await loadWasm()
  log(`BenchProducer.start(${transport})…`)
  producer = await wasm.BenchProducer.start(transport)
  ticketBox.value = producer.ticket
  stopBtn.disabled = false
  copyBtn.disabled = false
  log('bench producer ready — paste ticket into the consumer panel')
}

async function stopProducer(
  log: (...parts: unknown[]) => void,
  stopBtn: HTMLButtonElement,
  copyBtn: HTMLButtonElement,
) {
  if (!producer) {
    log('not producing')
    return
  }
  log('stopping…')
  const current = producer
  producer = null
  stopBtn.disabled = true
  copyBtn.disabled = true
  await current.stop()
  log('stopped')
}

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
  log: (...parts: unknown[]) => void,
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

/**
 * Serve a real file share, from OPFS handles or from picked-file snapshots.
 *
 * Everything after the source comes from the app: `startProducer` drives the
 * wasm `ShareProducer` exactly as the share buttons do. Only where the files
 * came from differs, which is the point — a test that built its own producer
 * would prove nothing about the one users get.
 */
async function startShare(
  mode: 'opfs' | 'snapshot',
  count: number,
  password: string,
  log: (...parts: unknown[]) => void,
  ticketBox: HTMLTextAreaElement,
  stopBtn: HTMLButtonElement,
  copyBtn: HTMLButtonElement,
): Promise<void> {
  if (share) {
    log('already sharing — stop first')
    return
  }
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
  share = { producer: started, root, added: 0 }
  ticketBox.value = started.ticket
  stopBtn.disabled = false
  copyBtn.disabled = false
  // Only a directory share rescans, so only that one can gain a file later.
  el<HTMLButtonElement>('share-add').disabled = root === null
  log(
    `file share ready — ${started.files} files, ${started.bytes} bytes`,
    started.passwordProtected ? '(password required)' : '',
  )
}

/**
 * Add a file to the share that is already running.
 *
 * The only way to exercise a browser producer's *live* path: `startProducer`
 * rescans its directory on a timer, so writing into that directory is what
 * makes it publish a new version and push a watch frame. A snapshot share has
 * no directory to write into and no rescan, which is the point of saying so
 * rather than failing quietly.
 */
async function addToShare(log: (...parts: unknown[]) => void): Promise<void> {
  if (!share) {
    log('not sharing')
    return
  }
  if (share.root === null) {
    log('a snapshot share cannot change — stop and start again to republish')
    return
  }
  const opfs = await navigator.storage.getDirectory()
  const dir = await opfs.getDirectoryHandle(share.root)
  const name = `added-${share.added}.txt`
  share.added += 1
  await writeOpfsFile(dir, name, `added while the share was running\n`)
  log(`wrote ${name} — the rescan should publish it within ~2s`)
}

/** Stop the file share and remove the OPFS directory, if it had one. */
async function stopShare(
  log: (...parts: unknown[]) => void,
  stopBtn: HTMLButtonElement,
  copyBtn: HTMLButtonElement,
): Promise<void> {
  if (!share) {
    log('not sharing')
    return
  }
  log('stopping…')
  const current = share
  share = null
  stopBtn.disabled = true
  copyBtn.disabled = true
  el<HTMLButtonElement>('share-add').disabled = true
  await current.producer.stop()
  if (current.root !== null && !(await removeShareDirectory(current.root))) {
    log('could not remove the OPFS directory')
  }
  log('stopped')
}

async function runBench(
  rawTicket: string,
  log: (...parts: unknown[]) => void,
): Promise<void> {
  const ticket = parseShareInput(rawTicket)
  if (!ticket) {
    log('no ticket in input')
    return
  }
  log('loading wasm…')
  const wasm = await loadWasm()
  const report = await wasm.ShareClient.bench(ticket, undefined, (status: {
    stage: string
    transport?: string
    connect_ms?: number
    duration_s?: number
    elapsed_s?: number
  }) => {
    switch (status.stage) {
      case 'connecting':
        log('Connecting', status.transport)
        break
      case 'connected':
        log('Connected', `${Number(status.connect_ms).toFixed(1)} ms (${status.transport})`)
        break
      case 'benching':
        log('Benching', `${status.duration_s}s`)
        break
      case 'progress':
        log('Benching', `${status.elapsed_s}s / ${status.duration_s}s`)
        break
      default:
        log('status', status)
    }
  })
  log('report', report)
}

type MeshPeer = Awaited<ReturnType<WasmModule['MeshPeer']['create']>>

let meshPeer: MeshPeer | null = null
let meshPoll: number | null = null

/** The link that drops another tab straight into this mesh. */
function meshUrl(id: string): string {
  return `${location.origin}${location.pathname}#mesh=${encodeURIComponent(id)}`
}

/**
 * Poll the two counters.
 *
 * Polling rather than a callback because both numbers are lock-free reads on
 * the wasm side — the roster is an atomic the event loop stores into, and the
 * direct count is a map length on the transport. Neither needs a hop into the
 * loop, so a timer is cheaper than plumbing an event channel out.
 */
function startMeshPoll(counts: HTMLElement) {
  if (meshPoll !== null) window.clearInterval(meshPoll)
  meshPoll = window.setInterval(() => {
    if (!meshPeer) return
    counts.textContent = `gossip ${meshPeer.peers_gossip} · direct ${meshPeer.peers_direct}/${meshPeer.max_direct}`
  }, 500)
}

async function meshStart(
  raw: string,
  log: (...parts: unknown[]) => void,
  ui: {
    idBox: HTMLTextAreaElement
    transport: HTMLSelectElement
    counts: HTMLElement
    nick: HTMLElement
    leave: HTMLButtonElement
    copy: HTMLButtonElement
  },
): Promise<void> {
  if (meshPeer) {
    log('already on a mesh — leave first')
    return
  }
  log('loading wasm…')
  const wasm = await loadWasm()
  const id = raw.trim()
  log(id ? 'joining…' : 'creating…')
  // Binding the endpoint and reaching a relay takes a few seconds; the button
  // stays live rather than freezing, and the log narrates.
  // `undefined` ⇒ every transport this target has; 'webrtc' pins the data
  // plane so a fallback shows up as a failure instead of passing quietly.
  const mode = ui.transport.value === 'dynamic' ? undefined : ui.transport.value
  const card = buildPeerCard({ role: 'consumer', transport: mode ?? 'webrtc' })
  meshPeer = id
    ? await wasm.MeshPeer.join(id, mode, card)
    : await wasm.MeshPeer.create(mode, card)
  ui.idBox.value = meshPeer.mesh_id
  ui.nick.textContent = `as <${meshPeer.nickname}>`
  ui.leave.disabled = false
  ui.copy.disabled = false
  startMeshPoll(ui.counts)
  log('up —', meshPeer.mesh_id)
  log('join URL —', meshUrl(meshPeer.mesh_id))
}

async function meshLeave(
  log: (...parts: unknown[]) => void,
  ui: { leave: HTMLButtonElement; copy: HTMLButtonElement; counts: HTMLElement },
) {
  if (!meshPeer) return
  const current = meshPeer
  meshPeer = null
  ui.leave.disabled = true
  ui.copy.disabled = true
  if (meshPoll !== null) {
    window.clearInterval(meshPoll)
    meshPoll = null
  }
  ui.counts.textContent = 'gossip 0 · direct 0/0'
  // Broadcasts `Left` so peers drop us now rather than on a silence timeout.
  await current.leave()
  log('left')
}

function main() {
  const txLog = logger(el('tx-log'))
  const rxLog = logger(el('rx-log'))
  const txTicket = el<HTMLTextAreaElement>('tx-ticket')
  const rxTicket = el<HTMLTextAreaElement>('rx-ticket')
  const txStart = el<HTMLButtonElement>('tx-start')
  const txStop = el<HTMLButtonElement>('tx-stop')
  const txCopy = el<HTMLButtonElement>('tx-copy')
  const txTransport = el<HTMLSelectElement>('tx-transport')
  const rxRun = el<HTMLButtonElement>('rx-run')
  const shareLog = logger(el('share-log'))
  const shareTicket = el<HTMLTextAreaElement>('share-ticket')
  const shareFiles = el<HTMLInputElement>('share-files')
  const shareMode = el<HTMLSelectElement>('share-mode')
  const sharePassword = el<HTMLInputElement>('share-password')
  const shareStart = el<HTMLButtonElement>('share-start')
  const shareAdd = el<HTMLButtonElement>('share-add')
  const shareStop = el<HTMLButtonElement>('share-stop')
  const shareCopy = el<HTMLButtonElement>('share-copy')

  shareStart.onclick = () => {
    const count = Number.parseInt(shareFiles.value, 10) || 1
    void startShare(
      shareMode.value === 'snapshot' ? 'snapshot' : 'opfs',
      count,
      sharePassword.value,
      shareLog,
      shareTicket,
      shareStop,
      shareCopy,
    ).catch((error) => {
      shareLog('FAILED', jsError(error))
    })
  }
  shareAdd.onclick = () => {
    void addToShare(shareLog).catch((error) => {
      shareLog('FAILED', jsError(error))
    })
  }
  shareStop.onclick = () => {
    void stopShare(shareLog, shareStop, shareCopy).catch((error) => {
      shareLog('FAILED', jsError(error))
    })
  }
  shareCopy.onclick = async () => {
    if (!shareTicket.value) return
    await navigator.clipboard.writeText(shareTicket.value)
    shareLog('ticket copied')
  }

  txStart.onclick = () => {
    void startProducer(txTransport.value, txLog, txTicket, txStop, txCopy).catch(
      (error) => {
        txLog('FAILED', jsError(error))
      },
    )
  }
  txStop.onclick = () => {
    void stopProducer(txLog, txStop, txCopy).catch((error) => {
      txLog('FAILED', jsError(error))
    })
  }
  txCopy.onclick = async () => {
    if (!txTicket.value) return
    await navigator.clipboard.writeText(txTicket.value)
    txLog('ticket copied')
  }
  rxRun.onclick = () => {
    void runBench(rxTicket.value, rxLog).catch((error) => {
      rxLog('FAILED', jsError(error))
    })
  }

  const meshLog = logger(el('mesh-log'))
  const meshUi = {
    idBox: el<HTMLTextAreaElement>('mesh-id'),
    transport: el<HTMLSelectElement>('mesh-transport'),
    counts: el<HTMLElement>('mesh-counts'),
    nick: el<HTMLElement>('mesh-nick'),
    leave: el<HTMLButtonElement>('mesh-leave'),
    copy: el<HTMLButtonElement>('mesh-copy'),
  }
  const meshGo = (raw: string) => {
    void meshStart(raw, meshLog, meshUi).catch((error) => {
      meshLog('FAILED', jsError(error))
    })
  }
  el<HTMLButtonElement>('mesh-create').onclick = () => meshGo('')
  el<HTMLButtonElement>('mesh-join').onclick = () => meshGo(meshUi.idBox.value)
  meshUi.leave.onclick = () => {
    void meshLeave(meshLog, meshUi).catch((error) => {
      meshLog('FAILED', jsError(error))
    })
  }
  meshUi.copy.onclick = async () => {
    if (!meshPeer) return
    await navigator.clipboard.writeText(meshUrl(meshPeer.mesh_id))
    meshLog('join URL copied')
  }

  // `#mesh=<id>` joins on load, which is what makes the link shareable —
  // open it in another tab, or on another machine, and that peer joins.
  const fragment = decodeURIComponent(location.hash.replace(/^#/, ''))
  if (fragment.startsWith('mesh=')) {
    const id = fragment.slice('mesh='.length).trim()
    if (id) {
      meshUi.idBox.value = id
      meshLog('joining from URL fragment…')
      meshGo(id)
    }
  }
}

main()
