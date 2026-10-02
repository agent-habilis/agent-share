/**
 * Who can serve which slot of a share.
 *
 * The data behind the availability grid: the same run-length encoding
 * `agent-share-proto`'s `serving` module writes onto a peer card, decoded here
 * so the info page can paint it the way a BitTorrent client paints pieces.
 *
 * A square is one *manifest slot* — one file — rather than one byte-chunk.
 * That is the granularity this protocol addresses bytes with, so it is the
 * granularity a peer can honestly answer for. Per-chunk availability would mean
 * republishing on every range that arrives, and the card rides a CRDT that keeps
 * its history.
 */

/** Marker a peer publishes when it holds every live slot. */
export const SERVING_ALL = '*'

/**
 * Prefix marking a base64 bitmap rather than a run list.
 *
 * A peer that seeds whatever it looked at holds scattered singletons, which is
 * the worst case for runs — ~70-100 of them used to overflow the frame and the
 * whole field was dropped, so a peer holding a hundred files advertised
 * nothing. A bitmap is one bit per slot however scattered, so it cannot fall
 * off that cliff.
 */
export const SERVING_BITMAP = '~'

const BASE64 = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/'

/**
 * Decode a bitmap into slot indices.
 *
 * Mirrors `agent_share_proto::serving`, including its tolerance: an unreadable
 * character ends the run rather than failing the field, so corruption costs the
 * tail and never invents a slot the peer does not hold. Over-claiming is the
 * dangerous direction — `missingSlots` uses this to say a file is *lost*.
 */
function decodeBitmap(body: string, total: number): number[] {
  const bytes: number[] = []
  let packed = 0
  let filled = 0
  for (const symbol of body) {
    const value = BASE64.indexOf(symbol)
    if (value === -1) break
    packed = (packed << 6) | value
    filled += 6
    if (filled >= 8) {
      filled -= 8
      bytes.push((packed >> filled) & 0xff)
    }
  }
  const held: number[] = []
  for (let slot = 0; slot < total; slot += 1) {
    const byte = bytes[slot >> 3]
    if (byte !== undefined && (byte & (1 << (slot % 8))) !== 0) held.push(slot)
  }
  return held
}

/** What one peer can serve, and of which tree. */
export interface PeerAvailability {
  /** Peer endpoint id. */
  id: string
  /** Sorted slot indices this peer says it holds. */
  held: number[]
  /**
   * Manifest fingerprint the indices refer to, when the peer published one.
   *
   * Load-bearing rather than decorative: a slot number means nothing without
   * agreeing which manifest it indexes into, so squares from peers on different
   * trees must never be drawn as though they line up.
   */
  tree: string | null
  /** True when the peer said `*` rather than enumerating. */
  complete: boolean
  /** The peer published nothing, so we cannot say either way. */
  unknown: boolean
}

/**
 * Decode a `serving` field into slot indices.
 *
 * Mirrors `agent_share_proto::serving::decode_serving`, including its
 * tolerance: a malformed run costs that run rather than the whole field, so one
 * bad range does not blank out a peer that is otherwise fine.
 */
export function decodeServing(encoded: string, total: number): number[] {
  const text = encoded.trim()
  if (text === SERVING_ALL) {
    return Array.from({ length: total }, (_, index) => index)
  }
  if (text.startsWith(SERVING_BITMAP)) {
    return decodeBitmap(text.slice(1), total)
  }
  const held: number[] = []
  for (const rawRun of text.split(',')) {
    const run = rawRun.trim()
    if (run === '') continue
    const dash = run.indexOf('-')
    if (dash === -1) {
      const only = Number.parseInt(run, 10)
      if (Number.isInteger(only) && only >= 0) held.push(only)
      continue
    }
    const start = Number.parseInt(run.slice(0, dash), 10)
    const end = Number.parseInt(run.slice(dash + 1), 10)
    if (!Number.isInteger(start) || !Number.isInteger(end)) continue
    if (start < 0 || end < start) continue
    for (let index = start; index <= end; index += 1) held.push(index)
  }
  return [...new Set(held)].sort((a, b) => a - b)
}

/** Build one peer's availability from its card fields. */
export function peerAvailability(
  id: string,
  serving: string | null | undefined,
  tree: string | null | undefined,
  total: number,
): PeerAvailability {
  if (serving == null || serving === '') {
    // Absent is *cannot vouch*, not *holds nothing*. A peer that has not worked
    // out its availability looks the same here, and the honest render is
    // "unknown" rather than an empty row implying it has nothing.
    //
    // Scattered holdings no longer land here: they used to overflow the frame
    // and drop the field, which is exactly what `SERVING_BITMAP` fixes.
    return { id, held: [], tree: tree ?? null, complete: false, unknown: true }
  }
  return {
    id,
    held: decodeServing(serving, total),
    tree: tree ?? null,
    complete: serving.trim() === SERVING_ALL,
    unknown: false,
  }
}

/** How many peers can serve each slot, for the "rarest" colouring. */
export function slotCoverage(peers: PeerAvailability[], total: number): number[] {
  const counts = new Array<number>(total).fill(0)
  for (const peer of peers) {
    if (peer.unknown) continue
    for (const slot of peer.held) {
      const current = counts[slot]
      if (current !== undefined) counts[slot] = current + 1
    }
  }
  return counts
}

/**
 * Slots no peer we can see holds.
 *
 * Worth surfacing rather than leaving to the eye: once the origin is gone,
 * these are the parts of the share that are *lost* unless somebody who has them
 * reappears, and that is the single most useful thing this view can tell you.
 */
export function missingSlots(peers: PeerAvailability[], total: number): number[] {
  const counts = slotCoverage(peers, total)
  const missing: number[] = []
  for (let slot = 0; slot < total; slot += 1) {
    if (counts[slot] === 0) missing.push(slot)
  }
  return missing
}
