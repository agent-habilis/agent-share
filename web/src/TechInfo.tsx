/**
 * Torrent-style session Info panel for the viewer.
 *
 * Renders in the app content area (not a modal). Counters and the getStats
 * sample both refresh every second — the sample interval is the averaging
 * window for the up/down rates, so it cannot lag the display.
 */

import { Stack, Text, roleVar } from 'moonspace-ui'
import { component, interval, listen, signal } from 'visage-dom'
import type { Ctx } from 'visage-dom'

import { formatIpWithFlag, isGeoLookupCandidate, lookupCountryCode } from './countryFlag/index.ts'
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
  /** Wire bytes on the selected ICE pair — includes SCTP/DTLS/STUN framing. */
  bytes_sent: number
  bytes_received: number
  /** Bytes/second since the previous sample; 0 until there are two. */
  up_bps: number
  down_bps: number
  /** Round-trip time in ms, or null when the pair has not been measured. */
  rtt_ms: number | null
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
    /** Why `dynamic` ended up on the relay. Null on a clean WebRTC connect. */
    mount_fallback_reason: string | null
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

/** `12.3 KB/s`, or `—` when there is nothing to report yet.
 *
 * Rounded before formatting: `humanBytes` only fixes the decimals above 1 KB,
 * so a raw bytes-per-second below that renders every float digit it has.
 */
function rate(bytesPerSecond: number): string {
  if (!Number.isFinite(bytesPerSecond) || bytesPerSecond <= 0) return '—'
  return `${humanBytes(Math.round(bytesPerSecond))}/s`
}

/** Ping to one decimal — sub-millisecond on a loopback pair is normal. */
function pingLabel(ms: number | null | undefined): string {
  if (ms == null || !Number.isFinite(ms)) return '—'
  return `${ms < 10 ? ms.toFixed(1) : Math.round(ms)} ms`
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
  // 1s: these are differenced counters, so the sampling interval *is* the
  // averaging window. At 5s a transfer that starts and ends between samples
  // never shows a rate at all.
  using _ips = interval(1000, () => {
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
                  {/*
                    Only for peers we hold a data channel with — a gossip-only
                    row has no candidate pair, so 0/0 there would read as
                    "nothing sent" rather than "not measured".
                  */}
                  {peer.bytes_sent > 0 || peer.bytes_received > 0 ? (
                    <>
                      <Text color="fgMuted">
                        up {rate(peer.up_bps)} · down {rate(peer.down_bps)}
                      </Text>
                      <Text color="fgMuted">ping {pingLabel(peer.rtt_ms)}</Text>
                      <Text color="fgMuted">
                        sent {humanBytes(peer.bytes_sent)} · received{' '}
                        {humanBytes(peer.bytes_received)}
                      </Text>
                    </>
                  ) : null}
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
          {/*
            The one line that answers "why is this on the relay?". Without it
            the fallback is a console warning nobody reading the pane can see.
          */}
          {info?.transfer.mount_fallback_reason ? (
            <Text color="warning">fell back: {info.transfer.mount_fallback_reason}</Text>
          ) : null}
        </Stack>
      </div>
    )
  }
})
