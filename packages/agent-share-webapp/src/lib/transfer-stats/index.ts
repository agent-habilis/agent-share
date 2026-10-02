/**
 * What the status bar says, derived from one link sample.
 *
 * The numbers arrive already differenced: `sample_link()` in the wasm client
 * owns the rate calculation, because the same `Meter` has to serve both byte
 * sources (QUIC path stats and WebRTC `getStats`) and one differencing rule is
 * the point of that facade. Everything here is presentation: peer arithmetic
 * and formatting.
 *
 * These are **wire** bytes on the mount connection, counted by the QUIC state
 * machine below the transport split, so they answer identically on the data
 * path and the WebRTC one. They are not payload: a download reads slightly
 * higher than the file, and they exclude mesh/gossip traffic, which rides
 * connections this client cannot reach.
 */

import { humanBytes } from '../tree.ts'

/** One meter, as `sample_link()` serialises it. */
export interface Meter {
  sent: number
  received: number
  up_bps: number
  down_bps: number
  rtt_ms: number | null
}

/** One network path of the mount connection. */
export interface Lane extends Meter {
  /** `webrtc` / `ip`. */
  label: string
  /** Whether this path is carrying application data right now. */
  selected: boolean
}

/** The whole `sample_link()` reading. */
export interface LinkSample {
  total: Meter
  lanes: Lane[]
}

/**
 * One tick of the session sampler: bytes and peers together.
 *
 * They travel as one value so the readout repaints once per tick rather than
 * twice — and so the two halves can never disagree about which second they
 * describe.
 */
export interface TransferSnapshot {
  link: LinkSample
  /** `peers_gossip` — the mesh roster, including us. See [`peerCounts`]. */
  gossip: number
  /** `peers_direct` — peers we hold a data channel with. */
  direct: number
}

/**
 * Cell widths for the status bar's value texts.
 *
 * The bar re-renders every second, so *nothing* in it may change width: a field
 * that grows by a character shoves every field to its right, once a second,
 * under whoever is reading it.
 *
 * Every formatter emits **exactly** its width — `000 KB/s` and `999 MB/s` are
 * both eight, `01/01` and `12/34` both five. No field holds slack, so `fit`
 * never pads and every gap in the row is the same one cell, which is what puts
 * the separators dead centre between their neighbours.
 */
export const RATE_CELLS = 8
export const PEERS_CELLS = 5

/**
 * Force `text` to exactly `cells` characters — padded right, or clipped.
 *
 * Clipping rather than growing is the whole point: a value that outruns its
 * column is a bug in the formatter, and the bar's job is to keep its shape
 * regardless. Losing a character is visible and local; reflowing the row is
 * neither. Monospace throughout, so one character is one cell.
 *
 * Callers must render the result with `white-space: pre`, or HTML will collapse
 * the padding and undo this.
 */
export function fit(text: string, cells: number): string {
  return text.length > cells ? text.slice(0, cells) : text.padEnd(cells)
}

export interface PeerCounts {
  /** Peers we hold a direct data channel with. */
  connected: number
  /** Peers we know about at all, excluding ourselves. */
  known: number
}

/**
 * Connected and known peer counts, both excluding us.
 *
 * `peers_gossip` counts the mesh roster *including* self, so a lone tab reads 1
 * — the CLI subtracts one for the same reason. The `max` is not decoration:
 * with the mesh down `peers_gossip` is 0 while a direct producer session may
 * still exist, and reporting `2/0` connected-of-known would be nonsense.
 */
export function peerCounts(gossip: number, direct: number): PeerCounts {
  // Every mount rides a data channel, so the mount peer is already in the
  // session registry and so in `direct`.
  const connected = Math.max(direct, 0)
  const known = Math.max(gossip - 1, connected, 0)
  return { connected, known }
}

/**
 * `02/05` — connected of known, a constant five cells.
 *
 * Two digits a side, zero-padded and clamped at 99, for the same reason the
 * rate carries three: a field that changes width drags the separator beside it.
 * Direct sessions cap at 16, so the clamp is unreachable on the left and all
 * but unreachable on the right.
 */
export function formatPeers(peers: PeerCounts): string {
  const pad = (n: number) => String(Math.min(Math.max(n, 0), 99)).padStart(2, '0')
  return `${pad(peers.connected)}/${pad(peers.known)}`
}

/**
 * `12.3 KB/s`, or `—` when there is nothing to report yet.
 *
 * For the `getStats` source, where a zero cannot be told apart from a peer that
 * has no candidate pair to ask — so it reports neither rather than claiming the
 * link is idle. [`formatSampledRate`] is the counterpart for a source that
 * always answers.
 *
 * Rounded before formatting: `humanBytes` only fixes the decimals above 1 KB,
 * so a raw bytes-per-second below that renders every float digit it has.
 */
export function formatRate(bytesPerSecond: number): string {
  if (!Number.isFinite(bytesPerSecond) || bytesPerSecond <= 0) return '—'
  return `${humanBytes(Math.round(bytesPerSecond))}/s`
}

/**
 * The scale starts at `KB`, not `B`.
 *
 * Every label is then two characters, so every rate is exactly eight — one
 * space between number and unit, and no padding anywhere. That is what lets the
 * separators sit centred; with `B` in the ladder a sub-kilobyte rate is one
 * character short and the dot after it inherits the slack.
 *
 * The cost is at the bottom of the range: idle keepalive traffic rounds away to
 * `000 KB/s`. Accepted deliberately — the readout is for watching a transfer,
 * and a few bytes a second of protocol chatter is not one.
 */
const RATE_UNITS = ['KB', 'MB', 'GB', 'TB'] as const

/**
 * A sampled rate as exactly three digits and a unit: `000 B/s`, `999 MB/s`,
 * `001 GB/s`.
 *
 * Two decisions, both about a number that is re-rendered every second.
 *
 * **Three digits, zero-padded, no decimal point.** The mantissa is always the
 * same width, so the readout cannot reflow as the rate climbs — `999 MB/s`
 * rolls straight to `001 GB/s` with nothing moving. A variable-width mantissa
 * (`9.9` → `10.1`) shifts every field to its right once a second, which is
 * exactly the jitter a status line must not have. The cost is precision at the
 * bottom of a unit: 1536 B/s reads `002 KB/s`.
 *
 * **`0 B/s` for a measured zero.** The QUIC meter always answers — there is no
 * "no path to ask" case once the connection is up — so a zero is a reading, not
 * a gap. Dashing it, the way [`formatRate`] must for `getStats`, would throw
 * away the difference between *nothing moved* and *nobody looked*. Only a rate
 * that cannot exist — negative or non-finite — still dashes.
 *
 * Promotion is decided on the **rounded** value, so 1023.6 B/s becomes
 * `001 KB/s` rather than the four-digit `1024 B/s`.
 */
export function formatSampledRate(bytesPerSecond: number): string {
  if (!Number.isFinite(bytesPerSecond) || bytesPerSecond < 0) return '—'
  // Straight into kilobytes — see RATE_UNITS for why the scale starts there.
  let value = bytesPerSecond / 1024
  let unit = 0
  while (unit < RATE_UNITS.length - 1 && Math.round(value) >= 1000) {
    value /= 1024
    unit += 1
  }
  const digits = String(Math.round(value)).padStart(3, '0')
  // The loop bounds `unit` to the table, but the checker cannot see that. The
  // fallback is the largest unit, which is the right answer for the overflow
  // that would have to happen to reach it.
  const label = RATE_UNITS[unit] ?? RATE_UNITS[RATE_UNITS.length - 1] ?? 'KB'
  return `${digits} ${label}/s`
}

/**
 * The per-lane split, for the tooltip: `webrtc* 41.2 MB · ip 0 B`.
 *
 * Received rather than sent, because that is the number a reader is checking
 * when they want to know which lane actually carried the share. The selected
 * lane is marked, since a connection can hold a path it is not using.
 */
export function laneSummary(lanes: readonly Lane[]): string {
  if (lanes.length === 0) return 'no paths'
  return lanes
    .map((lane) => `${lane.label}${lane.selected ? '*' : ''} ${humanBytes(lane.received)}`)
    .join(' · ')
}
