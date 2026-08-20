/**
 * What a page may ask of the session it is rendered inside.
 *
 * The pages under `agent-share-web` are mounted by the router's `Outlet`, which
 * passes no props, so this travels through context instead. Context is not
 * reactive on its own — the value is provided once and never replaced — so
 * every field here is either a signal, read inside a page's render thunk, or a
 * function whose identity never changes.
 */

import { context } from 'visage-dom'
import type { Ctx, ReadonlySignal, Signal } from 'visage-dom'

import type { Client } from '../../lib/client/index.ts'
import type { Progress } from '../../lib/download/index.ts'
import type { TransferSnapshot } from '../../lib/transfer-stats/index.ts'
import type { DirNode } from '../../lib/tree.ts'

export type Transfer = {
  kind: 'download' | 'mounting' | 'syncing'
  progress: Progress
  abort: AbortController
}

/** One tick's throughput, in bytes per second. See `SessionApi.history`. */
export interface RateSample {
  readonly up: number
  readonly down: number
}

export type ToastTone = 'error' | 'warning'

/**
 * Something gone wrong, on its way to the top bar.
 *
 * One at a time, and the newest wins: the bar has one row to give, and a queue
 * of stale failures is not worth the rows it would cost.
 */
export interface ToastMessage {
  readonly tone: ToastTone
  readonly message: string
}

/** What the top bar says while `kind` is in flight. */
export function transferLabel(kind: Transfer['kind']): string {
  return kind === 'download' ? 'downloading' : kind === 'mounting' ? 'mounting' : 'syncing'
}

/** A connected session with a listing. Pages only ever render inside one. */
export interface SessionReady {
  readonly client: Client
  readonly root: DirNode
  /** Manifest entries dropped for naming unsafe paths. */
  readonly skipped: number
}

export interface SessionApi {
  readonly ticket: string
  /**
   * `null` only in the window between a page mounting and the first listing —
   * the session renders its own connecting view instead of an outlet, so in
   * practice a page thunk sees this set.
   */
  readonly ready: ReadonlySignal<SessionReady | null>
  /** The column browser's selection. Survives a switch to another view. */
  readonly path: Signal<string[]>
  readonly held: ReadonlySignal<ReadonlySet<number>>
  readonly coverage: ReadonlySignal<ReadonlyMap<number, number>>
  readonly transfer: ReadonlySignal<Transfer | null>
  readonly sample: ReadonlySignal<TransferSnapshot | null>
  /**
   * The recent rate readings, oldest first — one per sampler tick, capped at a
   * minute's worth.
   *
   * `sample` is the instant; this is the shape of the last minute, which is the
   * only thing that distinguishes a stalled transfer from a slow one. Kept here
   * rather than in the view that draws it because the sampler is the one clock
   * allowed to advance, and a view that started its own would zero every rate
   * on screen.
   */
  readonly history: ReadonlySignal<readonly RateSample[]>
  /** Wall clock when this session was opened. Never changes. */
  readonly openedAt: number
  /**
   * Wall clock of the last tick on which mount bytes actually moved, or 0
   * before any have.
   *
   * Wire bytes, so a connection kept alive with nothing to say does not count
   * as activity — the whole point is telling an idle session from a live one.
   */
  readonly lastActivityAt: ReadonlySignal<number>
  /** The session sampler's tick, for views that re-read the client each one. */
  readonly tick: ReadonlySignal<number>
  readonly seeding: ReadonlySignal<boolean>
  /**
   * The last failure or notice, for as long as it is worth showing.
   *
   * Separate from `mountError` below, and deliberately: that is the record of
   * what went wrong, read by the Info panel long after the person has waved the
   * message away. This is only what is on screen.
   */
  readonly toast: ReadonlySignal<ToastMessage | null>
  /**
   * Why the last mirror stopped, or null.
   *
   * Mounting is the only action whose failure outlives its toast — a mirror
   * that stopped writing stays stopped, where a download that failed is over —
   * so it is the only one that keeps a signal rather than just raising a toast.
   */
  readonly mountError: ReadonlySignal<string | null>
  /**
   * Re-dialling a connection lost to a backgrounded tab. Browsing is
   * unaffected — the tree is local — so only the actions that reach the peer
   * read this.
   */
  readonly redialling: ReadonlySignal<boolean>
  readonly mounted: ReadonlySignal<boolean>
  /** ready / mounting / syncing / downloading / mounted / reconnecting. */
  readonly status: ReadonlySignal<string>
  /**
   * Raised by the Info page for as long as it is mounted.
   *
   * The peer-IP refresh is per-peer `getStats` on the main thread, competing
   * with the bulk transfer, so the session's one sampler runs it every tick
   * only while something renders it and every fifth tick otherwise. The page
   * cannot own the interval — see the sampler's comment on why there is
   * exactly one — so it raises a flag the sampler reads.
   */
  readonly wantsPeerIps: Signal<boolean>
  /** Take the message off the bar. Does not forget what went wrong. */
  dismissToast(): void
  /** Take the whole share, to a file the user picks. */
  downloadAll(): void
  /** Fetch the whole share into local storage so this tab can seed it. */
  seedShare(): void
  /** Mirror the share into a host directory, or drop the mirror. */
  mount(): void
  /** Adopt whatever this tab now holds into the seeder and advertise it. */
  publishHoldings(): void
  /**
   * The column browser's handlers.
   *
   * Identity-stable, and they have to be: a page thunk re-runs once per
   * transfer chunk, and `ColumnView`'s memo can only bail when every prop it
   * reads keeps its identity — an inline arrow re-made per run forces the whole
   * file tree to re-reconcile at chunk rate. `ColumnView`'s own test pins this.
   */
  readonly onPathChange: (next: string[]) => void
  readonly onDownload: () => void
  readonly onSeed: () => void
  readonly onPreview: () => void
}

export const SessionCtx = context<SessionApi>('agent-share.session')

export function useSession(ctx: Ctx): SessionApi {
  return ctx.inject(SessionCtx)
}
