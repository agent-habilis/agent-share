/**
 * Lab: synthetic OP_BENCH producer + consumer (transport set by producer).
 */

import { parseShareInput } from './ticket.ts'

function importWasm() {
  return import(
    '../../crates/agent-share-wasm-client/dist/web/agent_share_wasm_client.js'
  )
}

type WasmModule = Awaited<ReturnType<typeof importWasm>>
type BenchProducer = InstanceType<WasmModule['BenchProducer']>

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
}

main()
