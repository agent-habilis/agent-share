/**
 * Mesh peer: this tab as a full `agent-habilis-mesh` member — the same engine
 * the CLI runs.
 */

import { Button, Select, Stack, Text } from 'moonspace-dom'
import { component, interval, signal } from 'visage-dom'

import { Panel } from '../../components/Panel/index.tsx'
import { buildPeerCard } from '../../lib/peerCard/index.ts'
import { loadWasm, type WasmModule } from 'agent-share-wasm'
import { Field, TicketBox, createLog, jsError } from './parts.tsx'

type MeshPeer = Awaited<ReturnType<WasmModule['MeshPeer']['create']>>

/** Hoisted for its identity — see `producer.tsx`. */
const MODES = [
  { value: 'dynamic', label: 'dynamic' },
  { value: 'webrtc', label: 'webrtc-only' },
]

/** How often the two counters are re-read. */
const POLL_MS = 500

const NO_PEERS = { gossip: 0, direct: 0, max: 0 }

/** The link that drops another tab straight into this mesh. */
function meshUrl(id: string): string {
  return `${location.origin}${location.pathname}#mesh=${encodeURIComponent(id)}`
}

export const MeshPanel = component(function* () {
  const { view: logView, log } = createLog('mesh-log')
  const meshId = signal('')
  const nickname = signal('')
  const counts = signal(NO_PEERS)
  let peer: MeshPeer | null = null
  /** The live controls, read at click time — see `parts.tsx`. */
  let idEl: HTMLTextAreaElement | null = null
  let modeEl: HTMLSelectElement | null = null

  /**
   * Polling rather than a callback because both numbers are lock-free reads on
   * the wasm side — the roster is an atomic the event loop stores into, and the
   * direct count is a map length on the transport. Neither needs a hop into the
   * loop, so a timer is cheaper than plumbing an event channel out.
   */
  using _counts = interval(POLL_MS, () => {
    counts.value = peer
      ? { gossip: peer.peers_gossip, direct: peer.peers_direct, max: peer.max_direct }
      : NO_PEERS
  })

  async function join(raw: string): Promise<void> {
    if (peer) {
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
    const mode = modeEl?.value === 'dynamic' ? undefined : (modeEl?.value ?? 'webrtc')
    const card = buildPeerCard({ role: 'consumer', transport: mode ?? 'webrtc' })
    peer = id ? await wasm.MeshPeer.join(id, mode, card) : await wasm.MeshPeer.create(mode, card)
    meshId.value = peer.mesh_id
    nickname.value = `as <${peer.nickname}>`
    log('up —', peer.mesh_id)
    log('join URL —', meshUrl(peer.mesh_id))
  }

  async function leave(): Promise<void> {
    if (!peer) return
    const current = peer
    peer = null
    nickname.value = ''
    counts.value = NO_PEERS
    // Broadcasts `Left` so peers drop us now rather than on a silence timeout.
    await current.leave()
    log('left')
  }

  const go = (raw: string) => {
    void join(raw).catch((error: unknown) => log('FAILED', jsError(error)))
  }

  // `#mesh=<id>` joins on load, which is what makes the link shareable — open it
  // in another tab, or on another machine, and that peer joins.
  const fragment = decodeURIComponent(location.hash.replace(/^#/, ''))
  if (fragment.startsWith('mesh=')) {
    const id = fragment.slice('mesh='.length).trim()
    if (id) {
      meshId.value = id
      log('joining from URL fragment…')
      go(id)
    }
  }

  yield () => {
    const { gossip, direct, max } = counts.value
    const onMesh = nickname.value !== ''
    return (
      <Panel title="Mesh peer">
        <Stack direction="column" gap={1}>
          <Text color="fgMuted">
            <b>direct</b> counts live WebRTC data channels, <b>gossip</b> is the roster
            (including self). A #mesh=&lt;id&gt; fragment joins on load, so the link is
            shareable.
          </Text>
          <TicketBox
            id="mesh-id"
            placeholder="mesh id (blank ⇒ create a new mesh)"
            value={meshId.value}
            ref={(el) => {
              idEl = el
            }}
          />
          <Stack direction="row" gap={2} align="center" wrap>
            <Field label="transport">
              <Select
                id="mesh-transport"
                width={14}
                options={MODES}
                ref={(el) => {
                  modeEl = el
                }}
              />
            </Field>
            <Button id="mesh-create" variant="primary" onclick={() => go('')}>
              Create
            </Button>
            <Button id="mesh-join" variant="secondary" onclick={() => go(idEl?.value ?? '')}>
              Join
            </Button>
            <Button
              id="mesh-leave"
              variant="secondary"
              disabled={!onMesh}
              onclick={() => {
                void leave().catch((error: unknown) => log('FAILED', jsError(error)))
              }}
            >
              Leave
            </Button>
            <Button
              id="mesh-copy"
              variant="ghost"
              disabled={!onMesh}
              onclick={() => {
                void navigator.clipboard.writeText(meshUrl(meshId.peek()))
                log('join URL copied')
              }}
            >
              Copy join URL
            </Button>
          </Stack>
          <Stack direction="row" gap={2} align="center">
            <Text id="mesh-counts" weight="bold">
              gossip {gossip} · direct {direct}/{max}
            </Text>
            <Text id="mesh-nick" color="fgMuted">
              {nickname.value}
            </Text>
          </Stack>
          {logView}
        </Stack>
      </Panel>
    )
  }
})
