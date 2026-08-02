/**
 * Torrent-style session Info modal for the viewer.
 *
 * Portals to `document.body`, traps focus, and refreshes counters every second
 * while getStats IPs refresh on a slower cadence.
 */

import { Box, Button, Stack, Text } from 'moonspace-ui'
import { component, interval, listen, portal, signal } from 'visage-dom'
import type { Ctx } from 'visage-dom'

import { formatIpWithFlag, isGeoLookupCandidate, lookupCountryCode } from './countryFlag.ts'
import { humanBytes } from './tree.ts'

export interface InfoClient {
  info(): unknown
  refresh_peer_ips(): Promise<void>
}

export interface TechInfoModalProps {
  client: InfoClient
  fileCount: number
  totalBytes: number
  /** ready / mounting / syncing / downloading / mounted */
  status: string
  mounted: boolean
  mountError: string | null
  onClose: () => void
}

interface PeerRow {
  id: string
  role: string
  flags: string
  client: string
  version: string | null
  ip: string | null
  ip_kind: string | null
  proto: string
}

interface SessionInfo {
  general: {
    transport: string
    identity_fingerprint: string
    mesh_up: boolean
    nickname: string | null
    local_endpoint: string
    producer_endpoint: string
    connected_ms_ui: number
  }
  trackers: {
    relay_urls: string[]
    producer_reach: { mdns: boolean; dht: boolean; relay: string }
  }
  swarm: {
    peers_gossip: number
    peers_direct: number
    max_direct: number
    peers: PeerRow[]
  }
  transfer: {
    mount_mode: string
    mount_path: string
    mount_paths: string[]
  }
}

function formatDuration(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000))
  const h = Math.floor(total / 3600)
  const m = Math.floor((total % 3600) / 60)
  const s = total % 60
  if (h > 0) return `${h}h ${m}m ${s}s`
  if (m > 0) return `${m}m ${s}s`
  return `${s}s`
}

function dash(value: string | null | undefined): string {
  return value && value.length > 0 ? value : '—'
}

function shortId(id: string): string {
  if (id.length <= 20) return id
  return `${id.slice(0, 8)}…${id.slice(-8)}`
}

function capabilitiesLine(): string {
  const parts: string[] = []
  parts.push(typeof window.showDirectoryPicker === 'function' ? 'FSA' : 'no FSA')
  parts.push(typeof RTCPeerConnection === 'function' ? 'WebRTC' : 'no WebRTC')
  parts.push(window.isSecureContext ? 'secure' : 'insecure')
  return parts.join(' · ')
}

function readInfo(client: InfoClient): SessionInfo | null {
  try {
    return client.info() as SessionInfo
  } catch {
    return null
  }
}

function focusables(root: HTMLElement): HTMLElement[] {
  return [
    ...root.querySelectorAll<HTMLElement>(
      'button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])',
    ),
  ].filter((el) => !el.hasAttribute('disabled') && el.tabIndex !== -1)
}

export const TechInfoModal = component<TechInfoModalProps>(function* (props, ctx: Ctx) {
  const tick = signal(0)
  /** IP → ISO country code (or `null` after a failed / non-candidate lookup). */
  const countries = signal<Record<string, string | null>>({})
  const inflight = new Set<string>()
  let panel: HTMLElement | null = null
  let focused = false

  function requestCountry(ip: string): void {
    if (!isGeoLookupCandidate(ip)) return
    if (Object.hasOwn(countries.peek(), ip) || inflight.has(ip)) return
    inflight.add(ip)
    void lookupCountryCode(ip).then((code) => {
      inflight.delete(ip)
      if (ctx.aborted.aborted) return
      countries.value = { ...countries.peek(), [ip]: code }
    })
  }

  using _tick = interval(1000, () => {
    tick.value = tick.peek() + 1
  })
  using _ips = interval(5000, () => {
    void props.client.refresh_peer_ips()
  })
  void props.client.refresh_peer_ips()

  using _keys = listen(window, 'keydown', (event: Event) => {
    const keyEvent = event as KeyboardEvent
    if (keyEvent.key === 'Escape') {
      keyEvent.preventDefault()
      props.onClose()
      return
    }
    if (keyEvent.key !== 'Tab' || !panel) return
    const items = focusables(panel)
    if (items.length === 0) return
    const first = items[0]!
    const last = items[items.length - 1]!
    if (keyEvent.shiftKey && document.activeElement === first) {
      keyEvent.preventDefault()
      last.focus()
    } else if (!keyEvent.shiftKey && document.activeElement === last) {
      keyEvent.preventDefault()
      first.focus()
    }
  })

  ctx.aborted.addEventListener('abort', () => {
    panel = null
  })

  yield () => {
    tick.value
    const countryMap = countries.value
    const info = readInfo(props.client)
    const reach = info?.trackers.producer_reach
    const reachLine = reach
      ? [
          reach.mdns ? 'mdns' : null,
          reach.dht ? 'dht' : null,
          `relay=${reach.relay}`,
        ]
          .filter(Boolean)
          .join(' · ')
      : '—'

    const peerRows = (info?.swarm.peers ?? []).map((peer) => {
      if (peer.ip) requestCountry(peer.ip)
      return {
        ...peer,
        ipLabel: formatIpWithFlag(peer.ip, {
          countryCode: peer.ip ? (countryMap[peer.ip] ?? null) : null,
          kind: peer.ip_kind,
        }),
        // Full published label, e.g. `agent-share v0.1.0 (chrome, webrtc)`.
        clientLabel: peer.client,
      }
    })

    return portal(
      document.body,
      <div
        role="presentation"
        onclick={() => props.onClose()}
        style={{
          position: 'fixed',
          inset: 0,
          zIndex: 1000,
          display: 'flex',
          alignItems: 'center',
          justifyContent: 'center',
          background: 'color-mix(in srgb, var(--ms-bg-sunken) 72%, transparent)',
          padding: '2ch',
        }}
      >
        <div
          role="dialog"
          aria-modal="true"
          aria-labelledby="tech-info-title"
          ref={(el) => {
            panel = el as HTMLElement
            if (!focused) {
              focused = true
              panel.querySelector<HTMLElement>('#tech-info-close')?.focus()
            }
          }}
          onclick={(event: MouseEvent) => event.stopPropagation()}
          style={{
            maxWidth: '72ch',
            width: '100%',
            maxHeight: '90vh',
            overflowY: 'auto',
          }}
        >
          <Box border="line" background="bg" padX={2} padY={1}>
            <Stack direction="column" gap={1}>
              <Stack direction="row" gap={2} justify="between">
                <Text id="tech-info-title" weight="bold">
                  Info
                </Text>
                <Button id="tech-info-close" variant="secondary" onclick={() => props.onClose()}>
                  Close
                </Button>
              </Stack>

              <Text color="fgMuted" caps>
                General
              </Text>
              <Text color="fgMuted">
                transport {dash(info?.general.transport)} · {props.fileCount} files ·{' '}
                {humanBytes(props.totalBytes)} · direct{' '}
                {info?.swarm.peers_direct ?? 0}/{info?.swarm.max_direct ?? 0}
                {info?.general.mesh_up
                  ? ` · ${info.swarm.peers_gossip} on mesh`
                  : ' · mesh down'}{' '}
                · nickname {dash(info?.general.nickname)} · fingerprint{' '}
                {dash(info?.general.identity_fingerprint)} · status {props.status}
                {info?.general.mesh_up ? ' · mesh up' : ' · mesh down'} · connected{' '}
                {formatDuration(info?.general.connected_ms_ui ?? 0)}
              </Text>

              <Text color="fgMuted" caps>
                Trackers
              </Text>
              <Text color="fgMuted">
                {(info?.trackers.relay_urls.length ?? 0) === 0
                  ? 'no live relay URLs'
                  : info!.trackers.relay_urls.join(' · ')}{' '}
                · producer reach {reachLine}
              </Text>

              <Text color="fgMuted" caps>
                Peers
              </Text>
              <Text color="fgMuted">
                direct {info?.swarm.peers_direct ?? 0}/{info?.swarm.max_direct ?? 0} · gossip{' '}
                {info?.swarm.peers_gossip ?? 0}
              </Text>
              {peerRows.length === 0 ? (
                <Text color="fgSubtle">no peers</Text>
              ) : (
                <Stack direction="column" gap={1}>
                  {peerRows.map((peer) => (
                    <Stack key={peer.id} direction="column" gap={0}>
                      <Text color="fgMuted">client {peer.clientLabel}</Text>
                      <Text color="fgMuted">ip {peer.ipLabel}</Text>
                      {peer.flags ? (
                        <Text color="fgMuted">flags {peer.flags}</Text>
                      ) : null}
                      <Text color="fgMuted">proto {peer.proto}</Text>
                      <Text color="fgSubtle">
                        {peer.role} · {shortId(peer.id)}
                      </Text>
                    </Stack>
                  ))}
                </Stack>
              )}

              <Text color="fgMuted" caps>
                Transfer
              </Text>
              <Text color="fgMuted">
                mode {dash(info?.transfer.mount_mode)} → path{' '}
                {dash(info?.transfer.mount_path)} · paths{' '}
                {(info?.transfer.mount_paths.length ?? 0) > 0
                  ? info!.transfer.mount_paths.join(', ')
                  : '—'}{' '}
                · mounted {props.mounted ? 'yes' : 'no'}
                {props.mountError ? ` · last error: ${props.mountError}` : ''} ·{' '}
                {capabilitiesLine()}
              </Text>
            </Stack>
          </Box>
        </div>
      </div>,
    )
  }
})
