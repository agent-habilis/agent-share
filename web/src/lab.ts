/**
 * Lab: synthetic OP_BENCH producer + consumer (transport set by producer).
 */

// First, before anything can build a Disposable. See the file for why.
import './compat.ts'

import { buildPeerCard } from './peerCard/index.ts'
import { startProducer as startShareProducer, type ShareProducer } from './produce.ts'
import { parseShareInput } from './ticket/index.ts'
import { loadWasm, type WasmModule } from './wasm.ts'

type BenchProducer = Awaited<ReturnType<WasmModule['BenchProducer']['start']>>

let producer: BenchProducer | null = null

/** The file share this tab is serving, and the OPFS directory behind it. */
let share: { producer: ShareProducer; root: string } | null = null

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
 * A folder full of files, with no user gesture.
 *
 * `showDirectoryPicker()` is the app's way in and it requires a gesture, which
 * is why browser-side producing had no automated coverage at all. The origin
 * private file system is the same File System Access API without the picker:
 * `navigator.storage.getDirectory()` hands back a real
 * `FileSystemDirectoryHandle`, and `getFileHandle(…, {create: true})` real
 * `FileSystemFileHandle`s — which matters, because the wasm checks the type
 * (`parse_listing` does a `dyn_into::<FileSystemFileHandle>()`), so an object
 * that merely has `getFile()` is refused.
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
  const storage = navigator.storage
  if (typeof storage?.getDirectory !== 'function') {
    throw new Error('this browser has no origin private file system')
  }
  // Logged step by step: every one of these can hang or be denied depending on
  // the profile and the storage policy the browser was launched with, and a
  // silent stall here is indistinguishable from a slow producer.
  log('opening the origin private file system…')
  const opfs = await storage.getDirectory()
  log('opfs root acquired')
  // Unique per run, and removed on stop. OPFS is per-origin and outlives a
  // reload, so a fixed name would accumulate files across runs and quietly
  // change what a later run serves.
  const name = `lab-share-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`
  const handle = await opfs.getDirectoryHandle(name, { create: true })
  log(`directory ${name} created`)

  await writeFile(handle, 'blob.bin', 'x'.repeat(64 * 1024))
  log('blob.bin written')
  await writeFile(handle, 'empty.txt', '')
  const nested = await handle.getDirectoryHandle('nested', { create: true })
  await writeFile(nested, 'deep.txt', 'nested file\n')
  for (let index = 0; index < count; index += 1) {
    await writeFile(handle, `f${String(index).padStart(3, '0')}.txt`, `file ${index}\n`)
  }
  return { handle, name }
}

/** Write `text` to `name` under `dir`, creating it. */
async function writeFile(
  dir: FileSystemDirectoryHandle,
  name: string,
  text: string,
): Promise<void> {
  const file = await dir.getFileHandle(name, { create: true })
  if (typeof file.createWritable !== 'function') {
    throw new Error('this browser cannot write to the origin private file system')
  }
  const writable = await file.createWritable()
  await writable.write(text)
  await writable.close()
}

/**
 * Serve a real file share from OPFS.
 *
 * Everything after the root comes from the app: `startProducer` scans the
 * directory and drives the wasm `ShareProducer` exactly as **Add files/folder**
 * does. Only the origin of the handle differs, which is the point — a test that
 * built its own producer would prove nothing about the one users get.
 */
async function startShare(
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
  log(`seeding ${count} files into the origin private file system…`)
  const { handle, name } = await opfsShareRoot(count, log)
  log('seeded')
  log(`ShareProducer.start(${password ? 'with password' : 'no password'})…`)
  const started = await startShareProducer(handle, password || undefined)
  share = { producer: started, root: name }
  ticketBox.value = started.ticket
  stopBtn.disabled = false
  copyBtn.disabled = false
  log(
    `file share ready — ${started.files} files, ${started.bytes} bytes`,
    started.passwordProtected ? '(password required)' : '',
  )
}

/** Stop the file share and remove the OPFS directory behind it. */
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
  await current.producer.stop()
  // Best effort: a directory left behind costs disk, not correctness, and
  // failing the stop over it would be the worse trade.
  try {
    const opfs = await navigator.storage.getDirectory()
    await opfs.removeEntry(current.root, { recursive: true })
  } catch (error) {
    log('could not remove the OPFS directory', jsError(error))
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
  const sharePassword = el<HTMLInputElement>('share-password')
  const shareStart = el<HTMLButtonElement>('share-start')
  const shareStop = el<HTMLButtonElement>('share-stop')
  const shareCopy = el<HTMLButtonElement>('share-copy')

  shareStart.onclick = () => {
    const count = Number.parseInt(shareFiles.value, 10) || 1
    void startShare(
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
