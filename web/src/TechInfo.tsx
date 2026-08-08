/**
 * Torrent-style session Info panel for the viewer.
 *
 * Renders in the app content area (not a modal). It does not sample: `Session`
 * owns the app's one sampler and this pane repaints on the `tick` it publishes.
 * Two samplers would each compute the other's second reading over a few
 * milliseconds with no byte delta, overwriting the real rates with zero — the
 * counters would climb while the UI insisted nothing was moving.
 *
 * That tick is 1s, and the interval *is* the averaging window for the up/down
 * rates, since they come from differencing cumulative counters.
 */

import { Button, Stack, Text, t } from 'moonspace-dom'
import { component, interval, listen, signal } from 'visage-dom'
import type { ReadonlySignal } from 'visage-dom'

import { missingSlots, peerAvailability } from './availability.ts'
import { sortPeers } from './peers.ts'
import type { PeerAvailability } from './availability.ts'
import { formatIpWithFlag, isGeoLookupCandidate, lookupCountryCode } from './countryFlag/index.ts'
import { formatRate, laneSummary } from './transferStats.ts'
import type { LinkSample } from './transferStats.ts'
import { humanBytes } from './tree.ts'

export interface InfoClient {
  info(): unknown
  refresh_peer_ips(): Promise<void>
}

export interface TechInfoProps {
  client: InfoClient
  /** The session sampler's tick. Read during render, to repaint on each one. */
  tick: ReadonlySignal<number>
  fileCount: number
  totalBytes: number
  /** ready / mounting / syncing / downloading / mounted */
  status: string
  mounted: boolean
  mountError: string | null
  /** Reveal the dev tools. Set by `?dev=true`; off for anyone handed a link. */
  dev: boolean
  /** Close the mount connection, so the reconnect path can be exercised. */
  onKillConnection: () => void
  /** True while a reconnect is already running — nothing left to kill. */
  killDisabled: boolean
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
  /**
   * Which manifest slots this peer says it can serve — `*`, run-length ranges,
   * or absent when it has not said. See `availability.ts`.
   */
  serving: string | null
  /**
   * Manifest fingerprint those slot numbers index into. Squares from peers on
   * different trees do not line up and must not be drawn as though they do.
   */
  tree: string | null
}

/**
 * One peer's availability as a row of squares, the way a BitTorrent client
 * paints pieces.
 *
 * A square is one manifest slot — one file — because that is what this protocol
 * addresses bytes with and therefore what a peer can honestly answer for.
 *
 * Three states rather than two, and the third is the point: **filled** for a
 * slot the peer holds, **empty** for one it does not, and a single muted bar
 * for a peer that has published nothing. Drawing an all-empty row for the last
 * case would claim the peer has nothing, when what we actually know is that it
 * has not said.
 */
function AvailabilityRow(props: {
  peer: PeerAvailability
  total: number
  ourTree: string | null
}) {
  if (props.peer.unknown) {
    return <Text color="fgSubtle">chunks not published</Text>
  }
  // A slot index is meaningless across trees, so say so rather than paint
  // squares that appear to line up with everyone else's.
  if (props.ourTree && props.peer.tree && props.peer.tree !== props.ourTree) {
    return <Text color="fgSubtle">chunks on a different tree</Text>
  }
  const held = new Set(props.peer.held)
  const squares = Array.from({ length: props.total }, (_, slot) => held.has(slot))
  const filled = squares.filter(Boolean).length
  return (
    <Stack direction="column" gap={0}>
      <div
        style={{
          display: 'flex',
          flexWrap: 'wrap',
          gap: '1px',
          maxWidth: '40ch',
        }}
      >
        {squares.map((has, slot) => (
          <span
            key={slot}
            title={`slot ${slot}: ${has ? 'available' : 'missing'}`}
            style={{
              width: '0.8ch',
              height: '0.8ch',
              background: has ? t.accent : t.bgSunken,
              outline: has ? 'none' : `1px solid ${t.border}`,
            }}
          />
        ))}
      </div>
      <Text color="fgSubtle">
        {filled}/{props.total} slots{props.peer.complete ? ' · complete' : ''}
      </Text>
    </Stack>
  )
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
    /**
     * Wire bytes on the mount connection, per path and in total — the last
     * reading the session sampler took, never a fresh one. See `link.rs`.
     */
    link: LinkSample
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

export const TechInfo = component<TechInfoProps>(function* (props) {
  // The nested plain function below captures `ctx`; `this` would not reach it.
  const ctx = this
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

  // One sweep on open, so a pane opened between ticks is not blank for up to a
  // second. The recurring sweep belongs to the session's sampler.
  void props.client.refresh_peer_ips()

  using _keys = listen(window, 'keydown', (event: Event) => {
    const keyEvent = event as KeyboardEvent
    if (keyEvent.key === 'Escape') {
      keyEvent.preventDefault()
      props.onClose()
    }
  })

  yield () => {
    // Read, not used: this is the dependency that repaints the pane each time
    // the session samples.
    props.tick.value
    const countryMap = countries.value
    const info = readInfo(props.client)
    // Absent until the session sampler has taken its first reading.
    const link = info?.transfer.link ?? null
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

    // The manifest's file count, which the host already knows — a slot is a
    // file, so the grid has a width even before any peer publishes anything.
    const totalSlots = props.fileCount
    // Sorted once, here, because everything below reads from it. The wasm
    // client's own order is hash-derived and reshuffles on every poll — see
    // `peers.ts`.
    const peers = sortPeers(info?.swarm.peers ?? [])
    const ourTree = peers.find((peer) => peer.role === 'self')?.tree ?? null
    const availabilities = peers.map((peer) =>
      peerAvailability(peer.id, peer.serving, peer.tree, totalSlots),
    )
    const gaps = totalSlots > 0 ? missingSlots(availabilities, totalSlots) : []

    const peerRows = peers.map((peer) => {
      if (peer.ip) requestCountry(peer.ip)
      return {
        ...peer,
        availability: peerAvailability(peer.id, peer.serving, peer.tree, totalSlots),
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
        /*
          The one pane in the app that exists to be copied out of — fingerprints,
          relay URLs, peer addresses, fallback reasons. Selection is off
          app-wide (see `app.css`); this opts the whole surface back in.
        */
        class="selectable"
        style={{
          flex: 1,
          minHeight: 0,
          overflowY: 'auto',
          padding: '0 2ch calc(2 * var(--ms-row))',
          background: t.bg,
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
          {/*
            The one thing this view can tell you that a peer list cannot: which
            parts of the share nobody visible still holds. Once the origin is
            gone those slots are lost until somebody who has them reappears.
          */}
          {totalSlots > 0 ? (
            <Text color={gaps.length === 0 ? 'fgMuted' : 'fgSubtle'}>
              {gaps.length === 0
                ? `every slot is held by someone (${totalSlots})`
                : `${gaps.length} of ${totalSlots} slots held by nobody visible`}
            </Text>
          ) : null}
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
                        up {formatRate(peer.up_bps)} · down {formatRate(peer.down_bps)}
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
                  {totalSlots > 0 ? (
                    <AvailabilityRow
                      peer={peer.availability}
                      total={totalSlots}
                      ourTree={ourTree}
                    />
                  ) : null}
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
          {/*
            The mount connection's own counters, which — unlike the per-peer
            rows above — answer on the relay path too: they come from the QUIC
            state machine rather than a candidate pair. On a relay mount the
            peer rows can show a kilobyte of mesh chatter while megabytes of
            share moved right here, so this line is the one that reconciles.

            Wire bytes, and a smaller unit than the peer rows: QUIC sits below
            DTLS/SCTP on the WebRTC lane and below the relay's framing on the
            other. The two are not addable, which is why they are separate
            lines rather than one total.
          */}
          {link ? (
            <>
              <Text color="fgMuted">
                mount wire: down {formatRate(link.total.down_bps)} · up{' '}
                {formatRate(link.total.up_bps)} · received{' '}
                {humanBytes(link.total.received)} · sent {humanBytes(link.total.sent)}
              </Text>
              <Text color="fgSubtle">by path: {laneSummary(link.lanes)} received</Text>
            </>
          ) : null}

          {/*
            Behind `?dev=true`, and off for anyone handed a share link.

            Killing the connection is the only way to rehearse recovery on
            demand: the real failure — a backgrounded tab whose timers stretch
            past the keep-alive interval — happens only sometimes, so testing
            the reconnect used to mean idling a tab for minutes and hoping.

            Nothing here re-dials. Recovery is left to the ordinary triggers
            (press Download, or leave and return to the tab), because those are
            the paths worth testing and a self-healing button would skip them.
            So clicking this looks like it does nothing, which is the point.
          */}
          {props.dev ? (
            <>
              <Text color="fgMuted" caps>
                Dev
              </Text>
              <Stack direction="row" gap={1}>
                <Button
                  variant="secondary"
                  onclick={props.onKillConnection}
                  disabled={props.killDisabled}
                >
                  Kill connection
                </Button>
              </Stack>
              <Text color="fgSubtle">
                Closes the mount connection. Nothing visible happens until you
                press Download or leave and return to this tab — that is what
                triggers the reconnect.
              </Text>
            </>
          ) : null}
        </Stack>
      </div>
    )
  }
})
