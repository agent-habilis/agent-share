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
    // out its availability, or whose ranges were too scattered to fit a frame,
    // looks the same here — and in both cases the honest render is "unknown"
    // rather than an empty row implying it has nothing.
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
