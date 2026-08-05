/**
 * The middle of the top bar, in both the states it has.
 *
 * `TransferStatus` is the readout — `↓ rate · ↑ rate · ⇅ ratio · ⧉
 * connected/known`, refreshed once a second from the single sampler `Session`
 * owns. It takes the sample as a *signal* and reads it inside its own render
 * closure on purpose: if the session's render read it instead, the file list
 * would repaint every second along with these four numbers.
 *
 * Every field is exactly as wide as its formatter's output, and every gap is
 * one cell, so the separators sit centred and nothing moves as the numbers
 * change. Each field carries its own tooltip.
 *
 * The numbers are wire bytes on the mount connection, from the QUIC state
 * machine — see `transferStats.ts` for what that includes and excludes.
 *
 * `ReconnectingStatus` is what stands there instead while the session is
 * re-dialling. The two live in one file because they are one slot: whatever
 * occupies it must be exactly `oneRow` tall, and keeping both here is what
 * keeps that invariant in one place.
 */

import { Spinner, Text, oneRow } from 'moonspace-ui'
import { component } from 'visage-dom'
import type { ReadonlySignal } from 'visage-dom'
import { Style, css, raw } from 'visage-style'

import {
  PEERS_CELLS,
  RATE_CELLS,
  RATIO_CELLS,
  fit,
  formatBarRatio,
  formatPeers,
  formatRatio,
  formatSampledRate,
  laneSummary,
  peerCounts,
  ratio,
  type TransferSnapshot,
} from './transferStats.ts'
import { humanBytes } from './tree.ts'

export interface TransferStatusProps {
  /** The latest reading, or `null` until the first sample lands. */
  sample: ReadonlySignal<TransferSnapshot | null>
}

/**
 * One glyph and its value, in a box of fixed width.
 *
 * Three things make the layout invariant, and all three are needed:
 *
 * - `fit`, so the value is a constant number of characters whatever it says.
 * - `white-space: pre`, or HTML collapses `fit`'s trailing padding and hands
 *   the variable width straight back.
 * - `width`, not `minWidth`, with `overflow: hidden`. A minimum still lets an
 *   over-long value grow the box and push everything right of it; clipping a
 *   character instead keeps the row's shape no matter what a formatter does.
 *   With `fit` in front of it this should never fire — it is the backstop that
 *   makes "should" unnecessary.
 *
 * The separator is deliberately *not* in here. Pinned to the box's right edge
 * it ends up hard against the next field's glyph (`·↑`), reading as one mark;
 * sitting between two fixed boxes it gets air on both sides and still cannot
 * move.
 */
function Field({
  glyph,
  glyphCells = 1,
  label,
  title,
  value,
  cells,
}: {
  glyph: string
  /**
   * Cells reserved for the glyph.
   *
   * Not always one. `⧉` (U+29C9) is absent from the monospace font in use and
   * comes from a fallback that draws it at ~1.33 cells — measured, not assumed.
   * The box would then clip its own value, so it gets the extra cell here
   * rather than everyone paying for the widest possible glyph.
   *
   * Reserved, and then *centred* in what it reserved. Left to sit at the start
   * of its slot the unused third of a cell collects at the far end of the row
   * instead, and since that end is the row's last ink it drags the whole
   * readout 3px off the centre the grid works to put it on — measured at both
   * 1440px and 1920px, where the offset was identical and therefore structural
   * rather than a rounding artefact.
   */
  glyphCells?: number
  /** Spoken form, since the glyph alone says nothing to a screen reader. */
  label: string
  /** Hover text for this segment alone. */
  title: string
  value: string
  /** Cells for the value alone; the box adds the glyph and the gap. */
  cells: number
}) {
  return (
    <span
      title={title}
      style={{
        display: 'inline-flex',
        alignItems: 'baseline',
        gap: '1ch',
        width: `${cells + 1 + glyphCells}ch`,
        overflow: 'hidden',
        whiteSpace: 'pre',
      }}
    >
      <span
        aria-hidden="true"
        style={{ flex: 'none', width: `${glyphCells}ch`, textAlign: 'center' }}
      >
        <Text color="fgSubtle">{glyph}</Text>
      </span>
      <Text color="fgMuted" aria-label={`${label} ${value.trim()}`}>
        {fit(value, cells)}
      </Text>
    </span>
  )
}

function Separator() {
  return (
    <Text color="fgSubtle" aria-hidden="true">
      ·
    </Text>
  )
}

/**
 * `oneRow` and `inline-flex`, both load-bearing: this row is exactly one row
 * tall so nothing below it moves, and a default `inline` span would add
 * line-box leading and break that.
 *
 * The media query is the graceful exit. The readout wants ~62 cells and the
 * brand and actions want the rest; below roughly 1200px there is not room for
 * all three, and something has to give. Clipping the readout — which the grid
 * cell it sits in will do regardless, and must, because overlapping the buttons
 * is not an option — leaves a half-drawn number that reads as a rendering bug.
 * Withdrawing the whole row reads as a deliberate choice, which it is: on a
 * narrow window the actions are what a reader needs and the rates are not.
 */
const READOUT = css({
  ...oneRow,
  display: 'inline-flex',
  alignItems: 'center',
  gap: raw('1ch'),
  whiteSpace: 'nowrap',
  '@media (max-width: 1200px)': { display: 'none' },
})

/**
 * Hover text, one segment at a time.
 *
 * Each field answers for itself rather than sharing one tooltip for the row: a
 * reader hovering `⇅` wants to know what a ratio is here, not to re-read what a
 * wire byte is. Every one carries the session total behind the rate, since that
 * is the number the readout deliberately does not have room for.
 *
 * The closing line of each is the caveat that specific number needs — what the
 * bytes are counted at, which path carried them, why upload is small. Written
 * per-field because a single block listing all three made the reader work out
 * which sentence applied to the thing under the cursor.
 */
function lines(...parts: (string | null)[]): string {
  // `!== null`, not `filter(Boolean)`: an empty string is a deliberate blank
  // line separating the reading from the caveat below it, and `Boolean` throws
  // exactly those away — leaving the two paragraphs run together.
  return parts.filter((part) => part !== null).join('\n')
}

const PENDING = 'waiting for the first sample'

function describeDown(snapshot: TransferSnapshot | null): string {
  if (!snapshot) return PENDING
  const { total, lanes } = snapshot.link
  return lines(
    `Download — ${formatSampledRate(total.down_bps)}`,
    `${humanBytes(total.received)} received this session.`,
    `By path: ${laneSummary(lanes)}`,
    '',
    'Wire bytes, counted by the QUIC state machine below the transport split — so the relay path and the WebRTC one are measured the same way. Slightly more than the files themselves, and it excludes mesh traffic.',
  )
}

function describeUp(snapshot: TransferSnapshot | null): string {
  if (!snapshot) return PENDING
  const { total } = snapshot.link
  return lines(
    `Upload — ${formatSampledRate(total.up_bps)}`,
    `${humanBytes(total.sent)} sent this session.`,
    '',
    'Mostly acknowledgements for now: this tab holds the share’s bytes but does not serve them to other peers yet, so there is little to send.',
  )
}

function describeRatio(snapshot: TransferSnapshot | null): string {
  if (!snapshot) return PENDING
  const { total } = snapshot.link
  return lines(
    `Ratio — ${formatRatio(ratio(total.sent, total.received) ?? 0)}`,
    `Sent ÷ received: ${humanBytes(total.sent)} ÷ ${humanBytes(total.received)}.`,
    '',
    'How much this tab has given back against what it took. It climbs once browsers seed to each other.',
  )
}

function describePeers(snapshot: TransferSnapshot | null): string {
  if (!snapshot) return PENDING
  const peers = peerCounts(snapshot.gossip, snapshot.direct)
  return lines(
    `Peers — ${peers.connected} connected of ${peers.known} known`,
    'Connected: peers holding a direct WebRTC data channel with this tab.',
    'Known: members on this share’s mesh, not counting this tab.',
    peers.connected === 0 && peers.known > 0
      ? '\nNone direct — this session is riding the relay.'
      : null,
  )
}

export const TransferStatus = component<TransferStatusProps>(function* (props) {
  yield () => {
    const snapshot = props.sample.value
    const total = snapshot?.link.total
    const peers = peerCounts(snapshot?.gossip ?? 0, snapshot?.direct ?? 0)
    /*
      Zeros before the first sample, not dashes.

      The Info pane dashes an unmeasured value, and rightly — a peer with no
      candidate pair is not a peer sending nothing. This row is different: it
      appears only on a live connection, and the gap it would be covering is
      under a second on the way to a number that is genuinely zero when idle.
      A dash there reads as broken rather than quiet.
    */
    const down = formatSampledRate(total?.down_bps ?? 0)
    const up = formatSampledRate(total?.up_bps ?? 0)
    const share = formatBarRatio(total ? ratio(total.sent, total.received) : 0)

    return (
      <span>
        {Style(READOUT)}
        <Field
          glyph="↓"
          label="download"
          title={describeDown(snapshot)}
          cells={RATE_CELLS}
          value={down}
        />
        <Separator />
        <Field
          glyph="↑"
          label="upload"
          title={describeUp(snapshot)}
          cells={RATE_CELLS}
          value={up}
        />
        <Separator />
        <Field
          glyph="⇅"
          label="ratio"
          title={describeRatio(snapshot)}
          cells={RATIO_CELLS}
          value={share}
        />
        <Separator />
        {/*
          Two joined squares, not a filled circle. The separators are `·`, and a
          `●` is the same shape one size up — at 14px the marker read as another
          separator and the count looked like it was floating after a stray dot.
          This one also says *plural*, which is what a peer count is.

          `glyphCells={2}`: measured at ~1.33 cells, because no monospace font
          on hand carries U+29C9 and the fallback draws it wide.
        */}
        <Field
          glyph="⧉"
          glyphCells={2}
          label="peers"
          title={describePeers(snapshot)}
          cells={PEERS_CELLS}
          value={formatPeers(peers)}
        />
      </span>
    )
  }
})

/**
 * No media query, unlike `READOUT`.
 *
 * The readout withdraws below 1200px because it wants ~62 cells and there is
 * not room for it, the brand and the actions at once. This wants ~15 and is the
 * more important thing to say, so it stays at every width; the grid's
 * `minmax(0, auto)` middle track and its `overflow: hidden` wrapper handle the
 * extreme case on their own.
 *
 * No fixed width either. `Field` works hard for one because its value changes
 * every second; this string never changes, so the invariant is free.
 */
const RECONNECTING = css({
  ...oneRow,
  display: 'inline-flex',
  alignItems: 'center',
  gap: raw('1ch'),
  whiteSpace: 'nowrap',
})

/**
 * The centre slot while the session is re-dialling.
 *
 * It stands in for the readout rather than joining it. The four numbers are
 * sampled off the connection that just died, so leaving them up shows a healthy
 * page that cannot move a byte — which is the exact shape of the bug the
 * revival loop exists to fix, arriving as a rendering choice.
 *
 * The word is `aria-hidden` and the spinner carries it instead: `Spinner`
 * already renders a `role="status"` region with its label inside, so a screen
 * reader given both hears "reconnecting" twice. Same split as `Field`, where
 * the glyph is hidden and the value is labelled.
 */
export function ReconnectingStatus() {
  return (
    <span title="Reconnecting — the connection dropped, most often a backgrounded tab hitting the transport's idle timeout. The listing is local, so browsing still works; anything that needs the peer waits until it is back.">
      {Style(RECONNECTING)}
      <Spinner label="reconnecting" />
      <Text color="fgMuted" aria-hidden="true">
        reconnecting
      </Text>
    </span>
  )
}
