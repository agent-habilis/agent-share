/**
 * Bench producer: synthetic `OP_BENCH` echo/fill, no real files.
 */

import { Button, Select, Stack, Text } from 'moonspace-dom'
import { component, signal } from 'visage-dom'

import { Panel } from '../../components/Panel/index.tsx'
import { loadWasm, type WasmModule } from 'agent-share-wasm'
import { Field, TicketBox, createLog, jsError } from './parts.tsx'

type BenchProducer = Awaited<ReturnType<WasmModule['BenchProducer']['start']>>

/**
 * Hoisted so the list keeps its identity across repaints.
 *
 * A fresh array every render would re-run `Select`, and the option this page
 * asks a person to choose would be re-described while they are looking at it.
 */
const TRANSPORTS = [
  { value: 'webrtc', label: 'webrtc' },
  { value: 'relay', label: 'relay' },
]

export const ProducerPanel = component(function* () {
  const { view: logView, log } = createLog('tx-log')
  const ticket = signal('')
  const running = signal(false)
  let producer: BenchProducer | null = null
  /** The live `<select>`, read at click time — see `parts.tsx`. */
  let transportEl: HTMLSelectElement | null = null

  async function start(): Promise<void> {
    if (producer) {
      log('already producing — stop first')
      return
    }
    const transport = transportEl?.value ?? 'webrtc'
    log('loading wasm…')
    const wasm = await loadWasm()
    log(`BenchProducer.start(${transport})…`)
    producer = await wasm.BenchProducer.start(transport)
    ticket.value = producer.ticket
    running.value = true
    log('bench producer ready — paste ticket into the consumer panel')
  }

  async function stop(): Promise<void> {
    if (!producer) {
      log('not producing')
      return
    }
    log('stopping…')
    const current = producer
    producer = null
    running.value = false
    await current.stop()
    log('stopped')
  }

  yield () => (
    <Panel title="Bench producer">
      <Stack direction="column" gap={1}>
        <Text color="fgMuted">
          Synthetic OP_BENCH throughput and latency — echo/fill only, no real files. The
          transport is chosen here; the consumer reads it out of the ticket.
        </Text>
        <Stack direction="row" gap={2} align="center" wrap>
          <Field label="transport">
            <Select
              id="tx-transport"
              width={10}
              options={TRANSPORTS}
              ref={(el) => {
                transportEl = el
              }}
            />
          </Field>
          <Button
            id="tx-start"
            variant="primary"
            onclick={() => {
              void start().catch((error: unknown) => log('FAILED', jsError(error)))
            }}
          >
            Start
          </Button>
          <Button
            id="tx-stop"
            variant="secondary"
            disabled={!running.value}
            onclick={() => {
              void stop().catch((error: unknown) => log('FAILED', jsError(error)))
            }}
          >
            Stop
          </Button>
          <Button
            id="tx-copy"
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
        <TicketBox id="tx-ticket" placeholder="ticket appears here" value={ticket.value} readOnly />
        {logView}
      </Stack>
    </Panel>
  )
})
