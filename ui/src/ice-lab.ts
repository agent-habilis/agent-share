/**
 * Bare ICE / WebRTC lab: synthetic produce + ticket join, no directory picker.
 */

import { parseShareInput } from './ticket.ts'

function importWasm() {
  return import(
    '../../crates/agent-share-wasm-client/dist/web/agent_share_wasm_client.js'
  )
}

type WasmModule = Awaited<ReturnType<typeof importWasm>>
type ShareProducer = InstanceType<WasmModule['ShareProducer']>

let wasmModule: Promise<WasmModule> | null = null
let producer: ShareProducer | null = null

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
    console.log('[ice-lab]', ...parts)
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

async function startTransmit(
  log: (...parts: unknown[]) => void,
  ticketBox: HTMLTextAreaElement,
  stopBtn: HTMLButtonElement,
  copyBtn: HTMLButtonElement,
): Promise<void> {
  if (producer) {
    log('already sharing — stop first')
    return
  }
  log('loading wasm…')
  const wasm = await loadWasm()
  const body = new TextEncoder().encode(
    `hello from ice-lab\nstarted ${new Date().toISOString()}\n`,
  )
  const file = new File([body], 'hello.txt', { type: 'text/plain' })
  const listing = {
    dirs: [] as string[],
    files: [{ rel_path: 'hello.txt', size: file.size, file }],
  }
  log('ShareProducer.start (synthetic hello.txt)…')
  producer = await wasm.ShareProducer.start(listing)
  const ticket = producer.ticket
  ticketBox.value = ticket
  stopBtn.disabled = false
  copyBtn.disabled = false
  log('sharing', {
    transport: producer.transport,
    files: producer.files,
    bytes: Number(producer.bytes),
  })
  log('ticket ready — paste into the receiver tab')
}

async function stopTransmit(log: (...parts: unknown[]) => void, stopBtn: HTMLButtonElement, copyBtn: HTMLButtonElement) {
  if (!producer) {
    log('not sharing')
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

async function receive(
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
  log('ShareClient.connect…')
  const client = await wasm.ShareClient.connect(ticket)
  log('connected', { transport: client.transport })
  log('fetching manifest…')
  const manifest = (await client.manifest()) as {
    dirs: { rel_path: string }[]
    files: { rel_path: string; size: number }[]
  }
  log('manifest', manifest)
  const first = manifest.files?.[0]
  if (!first) {
    log('no files in share')
    return
  }
  const size = Number(first.size)
  log(`reading ${first.rel_path} (${size} bytes)…`)
  const bytes = await client.read(0, 0n, size)
  const text = new TextDecoder().decode(Uint8Array.from(bytes))
  log('read ok:\n' + text)
}

function main() {
  const txLog = logger(el('tx-log'))
  const rxLog = logger(el('rx-log'))
  const txTicket = el<HTMLTextAreaElement>('tx-ticket')
  const rxTicket = el<HTMLTextAreaElement>('rx-ticket')
  const txStart = el<HTMLButtonElement>('tx-start')
  const txStop = el<HTMLButtonElement>('tx-stop')
  const txCopy = el<HTMLButtonElement>('tx-copy')
  const rxConnect = el<HTMLButtonElement>('rx-connect')

  txStart.onclick = () => {
    void startTransmit(txLog, txTicket, txStop, txCopy).catch((error) => {
      txLog('FAILED', jsError(error))
    })
  }
  txStop.onclick = () => {
    void stopTransmit(txLog, txStop, txCopy).catch((error) => {
      txLog('FAILED', jsError(error))
    })
  }
  txCopy.onclick = async () => {
    if (!txTicket.value) return
    await navigator.clipboard.writeText(txTicket.value)
    txLog('ticket copied')
  }
  rxConnect.onclick = () => {
    void receive(rxTicket.value, rxLog).catch((error) => {
      rxLog('FAILED', jsError(error))
    })
  }

  const params = new URLSearchParams(location.search)
  const role = params.get('role')
  if (role === 'tx') {
    rxLog('open ?role=rx (or the other panel) in another tab')
  } else if (role === 'rx') {
    txLog('open ?role=tx (or the other panel) in another tab')
  }
}

main()
