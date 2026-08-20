/**
 * The wasm share client, and the cache that keeps one per ticket.
 *
 * The interface is the app's view of `ShareClient` — what the pages and the
 * download/mount helpers actually call, rather than everything wasm exports.
 */

import { buildPeerCard } from '../peer-card/index.ts'
import type { TransportMode } from '../ticket/index.ts'
import type { LinkSample } from '../transfer-stats/index.ts'
import type { Manifest } from '../tree.ts'
import { loadWasm } from 'agent-share-wasm'

export interface Client {
  readonly transport: string
  /**
   * The mount connection is gone and this client can only fail from here.
   *
   * A backgrounded tab loses it intermittently — the browser throttles timers
   * past the point where QUIC's keep-alive can beat the idle timeout — and
   * nothing announces it. Asked before acting, and on the way back to visible.
   */
  readonly closed: boolean
  /** Close the mount connection. Behind `?dev=true`; see `TechInfo`. */
  close_connection(): void
  /** Announce departure from the share's mesh. Safe to call more than once. */
  leave_mesh(): void
  /** The page-death farewell: purge the shared membership and broadcast. */
  shutdown_mesh(): void
  /** Members on the share's mesh, including us. 0 when the mesh is not up. */
  readonly peers_gossip: number
  /** Peers we hold a direct WebRTC data channel with. */
  readonly peers_direct: number
  readonly max_direct: number
  /**
   * How much of each known file this tab holds, as `{ index: fraction }`.
   *
   * Slots held whole are also in `held`; this adds the partial ones, which are
   * real and servable now that chunks are addressed individually.
   */
  coverage_map(): Promise<unknown>
  /**
   * Adopt what this tab holds into the seeder and advertise it.
   *
   * Called after a transfer rather than during: chunks land continuously, and
   * republishing per chunk would rewrite the peer card thousands of times for
   * one file — on a CRDT that keeps every revision.
   */
  republish_holdings(): Promise<void>
  /** Sync tech-info snapshot for the Info panel. */
  info(): unknown
  /** Refresh ICE remote-candidate addresses (slower cadence). */
  refresh_peer_ips(): Promise<void>
  /**
   * Sample the mount connection's wire counters. **Not a getter** — it
   * differences cumulative counters, so calling it twice in one tick zeroes the
   * rates. Exactly one driver may call it; see the sampler in `Session`.
   */
  sample_link(): LinkSample
  manifest(): Promise<Manifest>
  read(index: number, offset: bigint, len: number): Promise<Uint8Array>
  /** Subscribe to tree changes. Each call delivers the whole manifest. */
  watch(onManifest: (manifest: Manifest) => void): Promise<void>
  /**
   * Pull bytes into local storage so this tab can seed them.
   *
   * `only` names paths to take — a file, or a folder and everything under it.
   * Omit it for the whole share.
   */
  sync(only?: string[]): Promise<{
    files: number
    bytes: number
    verified: number
    unverified: number
    skipped: number
    held: number
  }>
  /** Manifest indices held in full, and therefore seedable. */
  readonly held: Uint32Array
  /** Recompute what is held from storage — what survived a reload. */
  refresh_held(): Promise<void>
}

/**
 * The prefix wasm puts on every credential failure, so the page can tell
 * "wrong password, ask again" from "this share is broken" without reading prose.
 * Mirrors `UNAUTHORIZED_PREFIX` in the wasm client.
 */
const UNAUTHORIZED_PREFIX = 'unauthorized:'

/** Whether `error` is the producer refusing the password rather than a fault. */
export function isUnauthorized(error: unknown): boolean {
  return String(error).includes(UNAUTHORIZED_PREFIX)
}

/**
 * One client per ticket *and* transport, shared across session mounts.
 *
 * Same reasoning one level up: a second dial for a ticket already being
 * negotiated would open a *second* WebRTC session for the same share — two ICE
 * runs, two data channels, one of them orphaned with no one left to close it.
 *
 * The transport is part of the key because it changes what gets dialled. Keyed
 * on the ticket alone, opening the same share with `?transport=webrtc` after a
 * default dial would hand back the cached dynamic client and quietly report on
 * the wrong session.
 */
const clients = new Map<string, Promise<Client>>()

/** Cache key for `clients`. Composed once so `connect` and `release` agree. */
export function clientKey(ticket: string, transport?: TransportMode): string {
  return `${transport ?? 'dynamic'} ${ticket}`
}

export function connect(
  ticket: string,
  transport?: TransportMode,
  originCapMs?: number,
  password?: string,
): Promise<Client> {
  const key = clientKey(ticket, transport)
  let client = clients.get(key)
  if (!client) {
    // Omit transport ⇒ dynamic (WebRTC preferred, iroh relay fallback);
    // `?transport=webrtc` pins the data path and makes ICE failure fatal.
    //
    // Peer card (runtime / version) is owned by this TS consumer, but its
    // transport is deliberately left for wasm to fill from the *settled* data
    // path. Publishing the requested mode instead would put "dynamic" on every
    // peer's roster, which says nothing about what is actually carrying bytes.
    //
    // `originCapMs` shortens the origin dial before the seeder fallback takes
    // over — the revival path passes a tight one, because it already *knows*
    // the origin just died and its whole attempt must fit the reconnect
    // budget. Fresh loads omit it and get the patient default.
    client = loadWasm().then(
      (wasm) =>
        wasm.ShareClient.connect(
          ticket,
          transport,
          buildPeerCard({ role: 'consumer' }),
          originCapMs,
          password,
        ) as unknown as Promise<Client>,
    )
    // Evict on failure so a retry (a re-entered hash, say) can dial again.
    client.catch(() => clients.delete(key))
    clients.set(key, client)
  }
  return client
}

/**
 * Give up this ticket's client: leave the share's mesh and evict the entry.
 *
 * Awaits the in-flight connect rather than skipping it. `ShareClient.connect`
 * starts a background mesh join that can land *after* it resolves, so a
 * session abandoned while still connecting may be about to acquire a live
 * membership broadcasting heartbeats — and the silence sweeper would never
 * evict it, because it is not silent. `leave_mesh` marks the client `Left`,
 * which the join task observes: a membership landing on a released client
 * says goodbye instead of installing itself. Skipping the await would skip
 * that mark. Changing the hash mid-connect was enough to leak one, once.
 *
 * Evicting is the other half. `leave_mesh` is one-way (it takes the mesh out
 * of the client), so a cached entry that has been left is a client that can
 * never rejoin: revisiting the ticket would hand back `max_direct === 0`, hide
 * the peer row for good, and start a second `watch` subscription against a
 * connection that already has one running and no way to cancel it.
 *
 * `owned` is the promise the caller was handed, and it is checked against the
 * cache before evicting: if a later session for the same ticket has already
 * replaced the entry, this one is releasing something it no longer owns.
 */
export function release(
  ticket: string,
  transport: TransportMode | undefined,
  owned: Promise<Client>,
): void {
  if (!evict(ticket, transport, owned)) return
  retire(owned)
}

/**
 * Drop `owned` from the cache — so a later `connect` for this ticket dials
 * fresh — without touching the client itself. `true` when it was ours to drop.
 *
 * Split from [`release`] for the revival path: a *seeding* client must keep
 * its mesh membership and mount handler alive while its replacement dials,
 * or every seeder of a dead-origin share tears itself down at once and there
 * is nobody left to reconnect *to*.
 */
export function evict(
  ticket: string,
  transport: TransportMode | undefined,
  owned: Promise<Client>,
): boolean {
  const key = clientKey(ticket, transport)
  if (clients.get(key) !== owned) return false
  clients.delete(key)
  return true
}

/**
 * Tear a discarded client down: leave the share's mesh, best-effort.
 *
 * Best-effort on *both* legs, which the two-argument form was not.
 *
 * A rejection handler covers `owned` failing to resolve, but a throw inside
 * `leave_mesh` rejects the promise `.then` hands back, and nothing was
 * watching that one. So a wasm-side fault during teardown reached the page
 * as an unhandled rejection — a crash overlay raised by a departure
 * announcement nobody was waiting on. Observed once as
 * `recursive use of an object detected which would lead to unsafe aliasing`
 * while a revival retried against a dead producer.
 *
 * Swallowing is right regardless of the cause: this client is already
 * discarded, the mesh drops silent members on its own, and there is nothing
 * the user could do about it. Logged at debug so the signal survives for
 * whoever chases the underlying fault, which is still unexplained.
 */
export function retire(owned: Promise<Client>): void {
  void owned
    .then((client) => client.leave_mesh())
    .catch((error: unknown) => {
      console.debug('[agent-share] leaving the mesh failed on teardown', error)
    })
}
