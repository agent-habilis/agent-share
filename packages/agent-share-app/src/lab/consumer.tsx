/**
 * Bench consumer: connect to a ticket and run the synthetic bench.
 *
 * The `report {…}` line this prints is what `tasks/src/bench/browser.rs` parses
 * out of the log, so the shape of that line is load-bearing.
 */

import { Button, Stack, Text } from 'moonspace-dom'
import { component } from 'visage-dom'

import { Panel } from 'agent-share-ui/Panel'
import { parseShareInput } from 'agent-share-core/ticket'
import { loadWasm } from 'agent-share-wasm'
import { TicketBox, createLog, jsError, type Log } from './parts.tsx'

async function runBench(rawTicket: string, log: Log): Promise<void> {
  const ticket = parseShareInput(rawTicket)
  if (!ticket) {
    log('no ticket in input')
    return
  }
  log('loading wasm…')
  const wasm = await loadWasm()
  const report = await wasm.ShareClient.bench(
    ticket,
    undefined,
    (status: {
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
    },
  )
  log('report', report)
}

export const ConsumerPanel = component(function* () {
  const { view: logView, log } = createLog('rx-log')
  /** The live box, read at click time — see `parts.tsx`. */
  let ticketEl: HTMLTextAreaElement | null = null

  yield () => (
    <Panel title="Bench consumer">
      <Stack direction="column" gap={1}>
        <Text color="fgMuted">Paste a ticket and run (30s after connect).</Text>
        <TicketBox
          id="rx-ticket"
          placeholder="ticket or share URL"
          ref={(el) => {
            ticketEl = el
          }}
        />
        <Stack direction="row" gap={2} align="center">
          <Button
            id="rx-run"
            variant="primary"
            onclick={() => {
              void runBench(ticketEl?.value ?? '', log).catch((error: unknown) =>
                log('FAILED', jsError(error)),
              )
            }}
          >
            Run bench
          </Button>
        </Stack>
        {logView}
      </Stack>
    </Panel>
  )
})
