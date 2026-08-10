/**
 * A stable order for the peer list.
 *
 * The wasm client hands back peers in whatever order its collections happened
 * to iterate, and those collections are hash-ordered and rebuilt constantly:
 * `direct_peer_ids` builds a fresh `HashSet` on every call, and the mesh's card
 * book is a `HashMap` that gets replaced wholesale on every meta event. Rust
 * seeds each hasher differently, so two collections holding the same peers come
 * out in different orders.
 *
 * The info pane re-reads that array once a second. Without a sort, rows visibly
 * swap places under whoever is reading them — measured live at 20 samples over
 * 45s producing two completely different arrangements of the same seven peers.
 * Keying the rendered rows does not help: DOM identity was already correct, and
 * what moved was the array.
 */

/** The fields ordering needs. The real row carries much more. */
export interface OrderablePeer {
  id: string
  role: string
}

/**
 * Where a row sits: self first, the producer second, everyone else after.
 *
 * Only those two are pinned, because they are the two fixed points of a share —
 * who you are, and who you got it from.
 */
function rank(role: string): number {
  if (role === 'self') return 0
  if (role === 'producer') return 1
  return 2
}

/**
 * Order peers for display, without mutating the input.
 *
 * Below the two pinned rows the order is by **endpoint id, not by role**. That
 * is deliberate: grouping direct peers above gossip-only ones would make a row
 * jump the moment a data channel opens or drops, which is the same jitter this
 * function exists to remove, arriving by a different route. A peer's connection
 * state belongs in its flags, which the row already shows.
 */
export function sortPeers<T extends OrderablePeer>(peers: readonly T[]): T[] {
  return [...peers].sort((left, right) => {
    const byRank = rank(left.role) - rank(right.role)
    if (byRank !== 0) return byRank
    // Plain code-unit comparison: ids are hex, and a locale-aware collation
    // would make the order depend on the reader's machine.
    if (left.id < right.id) return -1
    if (left.id > right.id) return 1
    return 0
  })
}
