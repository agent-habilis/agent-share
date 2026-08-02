/**
 * Torrent-style session Info panel for the viewer.
 *
 * Renders in the app content area (not a modal) and refreshes counters every
 * second while getStats IPs refresh on a slower cadence.
 */

import { Stack, Text, roleVar } from 'moonspace-ui'
import { component, interval, listen, signal } from 'visage-dom'
import type { Ctx } from 'visage-dom'

import { formatIpWithFlag, isGeoLookupCandidate, lookupCountryCode } from './countryFlag.ts'
import { humanBytes } from './tree.ts'

export interface InfoClient {
  info(): unknown
  refresh_peer_ips(): Promise<void>
}

export interface TechInfoProps {
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

export const TechInfo = component<TechInfoProps>(function* (props, ctx: Ctx) {
  const tick = signal(0)
  /** IP → ISO country code (or `null` after a failed / non-candidate lookup). */
  const countries = signal<Record<string, string | null>>({})
  const inflight = new Set<string>()

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
    }
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

    return (
      <div
        style={{
          flex: 1,
          minHeight: 0,
          overflowY: 'auto',
          padding: '0 2ch calc(2 * var(--ms-row))',
          background: roleVar.bg,
        }}
      >
        <Stack direction="column" gap={1}>
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
      </div>
    )
  }
})
