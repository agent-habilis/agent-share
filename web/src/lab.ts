/**
 * Lab: synthetic OP_BENCH producer + consumer (transport set by producer).
 */

// First, before anything can build a Disposable. See the file for why.
import './compat.ts'

import { buildPeerCard } from './peerCard/index.ts'
import { parseShareInput } from './ticket/index.ts'

function importWasm() {
  return import(
    '../../crates/agent-share-wasm-client/dist/web/agent_share_wasm_client.js'
  )
}

type WasmModule = Awaited<ReturnType<typeof importWasm>>
type BenchProducer = Awaited<ReturnType<WasmModule['BenchProducer']['start']>>

let wasmModule: Promise<WasmModule> | null = null
let producer: BenchProducer | null = null

function loadWasm(): Promise<WasmModule> {
  if (!wasmModule) {
    wasmModule = importWasm().then(async (module) => {
      await module.default()
      return module
    })
    wasmModule.catch(() => {
      wasmModule = null
    })
  }
  return wasmModule
}

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
