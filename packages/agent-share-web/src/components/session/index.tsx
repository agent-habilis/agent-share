/**
 * One dialled session for a fixed ticket, and the layout route that owns it.
 *
 * Everything that survives a view switch lives here: the client, the mesh
 * membership, the `watch` subscription, the single sampler and the column
 * browser's selection. The pages under `agent-share-web` render inside its outlet
 * and reach all of it through `SessionCtx` — so `/files` ↔ `/info` ↔
 * `/preview` swaps a page without redialling the share.
 *
 * `Session` is keyed on ticket + transport by `SessionLayout`, because a live
 * client cannot change either: a new key remounts, and every teardown hangs off
 * `ctx.aborted`. The router would otherwise keep one layout instance across a
 * ticket change, which is exactly what must not happen.
 */

import { component, computed, interval, signal } from 'visage-dom'
import type { Signal } from 'visage-dom'
import { Outlet, useLocation, useParams } from 'visage-router'

import { jittered, revivalOriginCapMs } from './backoff/index.ts'
import { forgetPassword, rememberPassword, rememberedPassword } from './password.ts'
import type { Revival } from './revival.ts'
import {
  SessionCtx,
  transferLabel,
  type SessionApi,
  type SessionReady,
  type ToastMessage,
  type ToastTone,
  type Transfer,
} from './session.ts'
import { Chrome } from '../chrome/index.tsx'
import { ColumnView } from '../column-view/index.tsx'
import { FailedBody } from '../failed-body/index.tsx'
import { LoadingBody } from '../loading-body/index.tsx'
import { PasswordGate } from '../password-gate/index.tsx'
import { Toast } from '../toast/index.tsx'
import { describe } from '../../lib/webmcp/result.ts'
import { useShareNav } from '../nav.ts'
import {
  clientKey,
  connect,
  evict,
  isUnauthorized,
  release,
  retire,
  type Client,
} from '../../lib/client/index.ts'
import {
  pickSaveTarget,
  singleFileStream,
  zipStream,
  type Progress,
  type SaveTarget,
} from '../../lib/download/index.ts'
import {
  disposeMount,
  emptySyncedState,
  pickMountRoot,
  shareStamp,
  syncMount,
  type MountSession,
  type SyncedState,
} from '../../lib/mount/index.ts'
import { parseTransport, type TransportMode } from '../../lib/ticket/index.ts'
import type { TransferSnapshot } from '../../lib/transfer-stats/index.ts'
import {
  buildTree,
  filesUnder,
  nodeAtPath,
  type FileNode,
  type Manifest,
} from '../../lib/tree.ts'
import { loadWasm } from 'agent-share-wasm'

type State =
  | { phase: 'idle' }
  | { phase: 'connecting' }
  | { phase: 'ready'; client: Client; manifest: Manifest }
  /**
   * The ticket says this share is password-protected and we do not have a
   * password it accepts. Its own phase rather than a `failed` variant: nothing
   * is broken and retrying changes nothing — the session is waiting on input.
   *
   * `error` is set on the second and later visits, when a password was tried
   * and refused.
   */
  | { phase: 'needs-password'; error?: string }
  | { phase: 'failed'; reason: string }

/**
 * How often the availability grid repaints while bytes are arriving.
 *
 * A second is well under the eye's threshold for "live" and well over the cost
 * of a coverage sweep, which reads the store once per known row.
 */
const HOLDINGS_REPAINT_MS = 1_000

/**
 * How long a message holds the top bar before it gives it back.
 *
 * It is holding the action row hostage the whole time — that is the price of
 * never adding a second row — so this is long enough to read a browser's own
 * wording twice and no longer. The close button is there for the impatient, and
 * the console keeps every message for anyone who looked away.
 */
const TOAST_MS = 8_000

/** Fall back to the deepest prefix that still exists in the manifest. */
function prunePath(current: string[], manifest: Manifest): string[] {
  const dirs = new Set(manifest.dirs.map((dir) => dir.rel_path))
  let depth = current.length
  while (depth > 0 && !dirs.has(current.slice(0, depth).join('/'))) depth -= 1
  return depth === current.length ? current : current.slice(0, depth)
}

const Session = component<{
  ticket: string
  transport?: TransportMode
}>(function* (props) {
  // Nested plain functions below capture `ctx`; `this` would not reach them.
  const ctx = this
  const nav = useShareNav(this)
  const state = signal<State>({ phase: 'connecting' })
  const path = signal<string[]>([])
  const transfer = signal<Transfer | null>(null)
  /**
   * Why the last mirror stopped.
   *
   * The one failure with a life after its toast, and so the one with a signal:
   * a mount that stopped writing is still stopped when the message has gone.
   * The Info panel reads it.
   */
  const mountError = signal<string | null>(null)
  /**
   * Re-dialling a connection that died while the tab was away.
   *
   * Deliberately *not* the `connecting` phase: that renders a full-page
   * loading view, and the manifest is still perfectly good — only the
   * connection is gone. Dropping the listing on every tab switch would read as
   * a fresh page load and lose the user's place for a heal they never asked
   * for. So the browser stays on screen and only the actions that need the
   * peer are held back.
   */
  const revival = signal<Revival | null>(null)
  /**
   * The tree this tab last seeded, read from its own storage while the dial
   * is still grinding. Rendered only inside the `connecting` phase — never a
   * phase of its own, so every `phase === 'ready'` guard stays correct — and
   * cleared the moment a live connection takes over. Browsing works; the two
   * actions that need a peer stay disabled.
   */
  const offlineManifest = signal<Manifest | null>(null)
  /** Non-null while a host directory is mounted for this session. */
  const mountSession = signal<MountSession | null>(null)
  /**
   * Manifest indices this tab holds in full, and so can seed.
   *
   * Mirrored out of the wasm client rather than tracked here: the store is the
   * only thing that knows what actually survived, and a set maintained in JS
   * would drift from it on every reload.
   */
  const held = signal<ReadonlySet<number>>(new Set())
  /** How much of each partially-held file this tab has. See `seeding.ts`. */
  const coverage = signal<ReadonlyMap<number, number>>(new Map())
  /** Whether a seed is in flight, so the button can say it is busy. */
  const seeding = signal(false)
  /**
   * The latest transfer reading, from the one sampler below.
   *
   * A signal rather than a prop computed during render, so only the readouts
   * that read it repaint each second — the file list must not.
   */
  const sample = signal<TransferSnapshot | null>(null)
  const openedAt = Date.now()
  const lastActivityAt = signal(0)
  /** Wire bytes at the previous tick, to tell movement from a quiet keep-alive. */
  let movedBytes = 0
  /** Bumped by the same tick, for views that re-read `info()` rather than this. */
  const tick = signal(0)
  /** Set by the Info page while it is mounted. See `SessionApi.wantsPeerIps`. */
  const wantsPeerIps = signal(false)

  /**
   * What the top bar is saying instead of itself. See `Toast`.
   *
   * It clears itself because it costs the action row to stay: the bar has one
   * row, so a message nobody waves away hides `Download` for as long as the tab
   * is open. Long enough to read a browser's own sentence, and no longer.
   *
   * Nothing on screen outlives it except `mountError`, which the Info panel
   * shows as `last error`. The console is where all three are kept.
   */
  const toast = signal<ToastMessage | null>(null)
  let hideTimer: ReturnType<typeof setTimeout> | undefined

  /**
   * Put a message on the bar, and the same message on the console.
   *
   * Both, always. The bar is for the person and clears itself after a few
   * seconds; the console is the copy that is still there when someone comes to
   * ask what happened. `cause` is the error object where there is one — the
   * console is the only one of the two that can keep it, stack and all, and the
   * stack is what names the action that failed.
   */
  function raiseToast(tone: ToastTone, message: string, cause?: unknown): void {
    toast.value = { tone, message }
    if (cause === undefined) console.warn(`[share] ${message}`)
    else console.warn(`[share] ${message}`, cause)
    clearTimeout(hideTimer)
    hideTimer = setTimeout(() => (toast.value = null), TOAST_MS)
  }

  function dismissToast(): void {
    clearTimeout(hideTimer)
    toast.value = null
  }

  ctx.aborted.addEventListener('abort', () => clearTimeout(hideTimer))

  /** An action failed: say so on the bar and on the console. */
  function reportFailure(error: unknown): void {
    raiseToast('error', describe(error), error)
  }

  /**
   * The same, for the one failure that outlives its toast.
   *
   * Only mounting keeps a record, because only mounting is still true after the
   * message has cleared: a mirror that stopped writing stays stopped, where a
   * download that failed is simply over. The Info panel's `last error` is the
   * reader, and a signal nothing reads is a signal that will drift.
   */
  function reportMountFailure(error: unknown): void {
    mountError.value = describe(error)
    reportFailure(error)
  }

  /**
   * Say once that the peer's manifest named paths we refuse to write.
   *
   * Once, not once per manifest: `watch` re-delivers the listing whenever the
   * producer touches a file, and a notice that reappears every time is a notice
   * that gets dismissed without being read. Only a change in the count is new
   * information.
   */
  let lastSkipped = 0
  function noteSkipped(skipped: number): void {
    if (skipped === lastSkipped) return
    lastSkipped = skipped
    if (skipped === 0) return
    raiseToast('warning', `${skipped} entries hidden — unsafe paths in the peer's manifest`)
  }

  /**
   * `buildTree`, but at most once per manifest.
   *
   * Three readers want the same tree — the `tree` computed the pages render,
   * the offline one, and the mount sync — and a delivery replaces the manifest
   * object wholesale, so its identity is a sound key. It also gives the callers
   * that hold a manifest a way to read the tree *now*: a computed cannot serve
   * them, because a write only marks its dependents stale on the next
   * microtask, so `tree.peek()` on the line after `state.value = …` still
   * answers for the manifest before it.
   */
  let treeFrom: Manifest | null = null
  let treeBuilt: ReturnType<typeof buildTree> | null = null
  function treeOf(manifest: Manifest): ReturnType<typeof buildTree> {
    if (manifest !== treeFrom) {
      treeFrom = manifest
      treeBuilt = buildTree(manifest)
    }
    return treeBuilt!
  }

  /*
    The app's single sampler.

    Both calls difference cumulative counters, so the interval *is* the
    averaging window for every rate on screen — at 5s a transfer that starts and
    ends between samples never shows a rate at all. And there is exactly one of
    them: two samplers would each compute the other's second reading over a few
    milliseconds with no byte delta, overwriting real rates with zero. The Info
    pane used to own its own pair of intervals; it now reads what this produces.
  */
  using _sampler = interval(1000, () => {
    const current = state.peek()
    if (current.phase !== 'ready') return
    // Until the swap, `current.client` is the retiring one. Its data channels
    // may still count as open — after a sleep the browser fires no `close`
    // for a channel whose QUIC died underneath — so its numbers describe a
    // connection this tab no longer has. The header shows the redial instead.
    if (revival.peek()) return
    // getStats, for the per-peer rows and the ICE addresses behind them.
    // Per-peer round-trips on the main thread, competing with the bulk
    // transfer — and the wasm side asks for a slower cadence. With the Info
    // pane open it runs every tick (it is what the pane renders); closed,
    // every fifth is enough to keep the cached addresses warm for the
    // pane's first paint.
    if (wantsPeerIps.peek() || tick.peek() % 5 === 0) {
      void current.client.refresh_peer_ips()
    }
    // QUIC, for the whole connection — the half that answers on the relay path,
    // where there is no candidate pair to ask.
    const link = current.client.sample_link()
    sample.value = {
      link,
      gossip: current.client.peers_gossip,
      direct: current.client.peers_direct,
      // A live mount on the relay (or IP) is a connected peer the direct
      // count cannot see — the WebRTC-path mount is already inside it.
      relayPeer: !current.client.closed && current.client.transport !== 'webrtc',
    }
    const moved = link.total.sent + link.total.received
    if (moved > movedBytes) lastActivityAt.value = Date.now()
    movedBytes = moved
    tick.value = tick.peek() + 1
  })

  let synced: SyncedState = emptySyncedState()
  let syncing = false
  let syncDirty = false

  async function clearMount(): Promise<void> {
    const session = mountSession.peek()
    mountSession.value = null
    synced = emptySyncedState()
    if (session) await disposeMount(session)
  }

  async function runSync(label: 'mounting' | 'syncing'): Promise<void> {
    const session = mountSession.peek()
    const current = state.peek()
    if (!session || current.phase !== 'ready') return
    if (syncing) {
      syncDirty = true
      return
    }
    syncing = true
    const abort = new AbortController()
    let pass: 'mounting' | 'syncing' = label
    try {
      do {
        syncDirty = false
        // Re-read the latest ready state each pass — a watch may have landed
        // while the previous write was in flight.
        const latest = state.peek()
        if (latest.phase !== 'ready' || mountSession.peek() !== session) break
        if (abort.signal.aborted) break
        const latestTree = treeOf(latest.manifest)
        const latestFiles = filesUnder(latestTree.root)
        transfer.value = { kind: pass, progress: { done: 0, total: 0 }, abort }
        synced = await syncMount(
          session.root,
          latest.client,
          latestFiles,
          latest.manifest.dirs,
          synced,
          (progress) => {
            transfer.value = { kind: pass, progress, abort }
          },
          abort.signal,
        )
        // After the first full mirror, later passes are incremental syncs.
        pass = 'syncing'
      } while (
        syncDirty &&
        mountSession.peek() === session &&
        !ctx.aborted.aborted &&
        !abort.signal.aborted
      )
      // Cancel during the initial mirror drops the half-written folder.
      if (abort.signal.aborted && label === 'mounting') {
        await clearMount()
      }
    } catch (error) {
      if (error instanceof DOMException && error.name === 'AbortError') {
        if (label === 'mounting') await clearMount()
      } else if (!ctx.aborted.aborted) {
        reportMountFailure(error)
        await clearMount()
      }
    } finally {
      syncing = false
      if (transfer.peek()?.kind === 'mounting' || transfer.peek()?.kind === 'syncing') {
        transfer.value = null
      }
    }
  }

  /**
   * The password this session presents, if the share wants one.
   *
   * Seeded from the tab's memory so a refresh or a `/files` ↔ `/info` switch
   * does not re-prompt. `undefined` on an ordinary share, where it is simply
   * never read.
   */
  let password = rememberedPassword(props.ticket)

  // Dial now, and register the release before the first await. A ticket change
  // unmounts this session; the mesh membership belongs to it, so it goes too —
  // and it has to go even when the abort lands *during* the connect, which is
  // why this holds the promise rather than a client we may not have yet.
  let pending = connect(props.ticket, props.transport, undefined, password)
  ctx.aborted.addEventListener('abort', () => release(props.ticket, props.transport, pending))

  // A tab that seeded this share before can show its tree in about a second:
  // the peek reads this tab's own fingerprint-checked sidecar, no peer
  // involved. Best-effort and racing the dial on purpose — a fast connect
  // flips the phase first and the result is simply never shown.
  void loadWasm()
    .then((wasm) => wasm.ShareClient.peek_persisted_manifest(props.ticket, password))
    .then((manifest) => {
      if (ctx.aborted.aborted || !manifest) return
      if (state.peek().phase === 'connecting') {
        offlineManifest.value = manifest as Manifest
        noteSkipped(treeOf(manifest as Manifest).skipped)
      }
    })
    .catch(() => {
      // A malformed ticket fails the dial too, with a better message.
    })

  // Announce departure while the page still exists. Without this the tab
  // lingers on every peer's roster until the silence sweeper evicts it —
  // which showed up immediately in testing as a share reporting more
  // members than there were processes.
  //
  // `pagehide`, not `beforeunload`: the latter is unreliable on mobile.
  // Registered once for the session rather than per connection: a reconnect
  // would otherwise stack a second listener holding a stale client.
  const onHide = (event: PageTransitionEvent) => {
    // `persisted` means the page is going into the bfcache, and may come
    // straight back on Back with its JS state intact — no remount, no
    // hashchange, nothing that would rejoin. Leaving here took the tab off
    // every roster while it still looked fully connected, and the peer row
    // silently disappeared because `max_direct` fell to 0.
    if (event.persisted) return
    const current = state.peek()
    if (current.phase === 'ready') current.client.shutdown_mesh()
  }
  window.addEventListener('pagehide', onHide)
  ctx.aborted.addEventListener('abort', () => {
    window.removeEventListener('pagehide', onHide)
  })

  /** Bring a connected client into view: listing first, then live updates. */
  async function bringUp(promise: Promise<Client>): Promise<void> {
    const client = await promise
    if (ctx.aborted.aborted) return
    const manifest = await client.manifest()
    if (ctx.aborted.aborted) return

    /** Put a delivered listing on screen — the first one and every later one. */
    const show = (next: Manifest): void => {
      path.value = prunePath(path.peek(), next)
      state.value = { phase: 'ready', client, manifest: next }
      noteSkipped(treeOf(next).skipped)
    }

    show(manifest)
    // The live tree owns the screen now; the peeked one has done its job.
    offlineManifest.value = null
    // What survived a previous visit. Reads storage without creating any, so
    // a tab that only browses leaves nothing behind. Inside `bringUp` rather
    // than beside the first call, so a revived connection re-reads it too —
    // the client is new after a redial and its held set starts empty.
    void client.refresh_held().then(() => {
      if (!ctx.aborted.aborted) refreshHeld(client)
    })
    await client.watch((next) => {
      if (ctx.aborted.aborted) return
      show(next)
      // Re-arm as a seeder for the version we just took. Without this a tab
      // that verified v2 keeps answering other peers with v1's envelope, and a
      // change stops at the first hop — the share propagates one peer deep and
      // no further. It also pushes the new version on to anyone watching *this*
      // tab, which is what makes propagation transitive.
      publishHoldings(client)
      if (mountSession.peek()) void runSync('syncing')
    })
  }

  /**
   * Errors no amount of retrying can fix: the ticket itself is unusable.
   * Everything else — dead origin, empty mesh, relay hiccup — is a share
   * waiting for a peer, and an open tab waits with it.
   */
  function permanentConnectError(error: unknown): boolean {
    const text = String(error)
    return (
      text.includes('decode ticket') ||
      text.includes('ticket has no relay') ||
      text.includes('unknown transport')
    )
  }

  /**
   * Dial until the share comes up, or until something says stop.
   *
   * Re-entered by the password gate: a refused credential ends this loop
   * rather than retrying it — the same password would be refused every time —
   * and submitting a new one starts it again.
   */
  async function dialUntilReady(): Promise<void> {
    // Literal backoff bounds: the shared constants are declared later in this
    // body, and this loop starts synchronously — before they initialize.
    let backoff = 1_000
    while (!ctx.aborted.aborted) {
      try {
        await bringUp(pending)
        return
      } catch (error) {
        if (ctx.aborted.aborted) return
        // The producer refused the credential — or the ticket says it wants one
        // and we have none. Retrying is pointless and would spend ~100 ms of
        // Argon2id per attempt; what is missing is a person, so ask for one.
        if (isUnauthorized(error)) {
          const stale = password !== undefined
          if (stale) forgetPassword(props.ticket)
          password = undefined
          state.value = {
            phase: 'needs-password',
            error: stale ? 'That password does not open this share.' : undefined,
          }
          release(props.ticket, props.transport, pending)
          return
        }
        if (permanentConnectError(error)) {
          state.value = { phase: 'failed', reason: String(error) }
          return
        }
        console.debug('[agent-share] connect failed; retrying', error)
        await new Promise((resolve) => setTimeout(resolve, jittered(backoff)))
        backoff = Math.min(backoff * 2, 10_000)
        release(props.ticket, props.transport, pending)
        pending = connect(props.ticket, props.transport, undefined, password)
      }
    }
  }

  /** The gate's submit: remember the password and dial again from the top. */
  function submitPassword(entered: string): void {
    rememberPassword(props.ticket, entered)
    password = entered
    state.value = { phase: 'connecting' }
    pending = connect(props.ticket, props.transport, undefined, password)
    void dialUntilReady()
  }

  void dialUntilReady()

  /** In-flight revival, so concurrent callers share one dial. */
  let revivalInFlight: Promise<void> | null = null

  /**
   * Dial again if the connection died while we were not looking.
   *
   * A backgrounded tab loses the connection to QUIC's idle timeout — the
   * browser throttles timers past the keep-alive interval — and nothing
   * reports it: `watch`'s follower just returns. The tab then looks perfectly
   * connected and fails every action until it is reloaded, which is the bug
   * this exists to remove.
   *
   * Started the moment the death is noticed — by the poll below, by the tab
   * coming back to the foreground, or by an action that needs the peer.
   *
   * **Forever, on purpose.** There is no give-up deadline: an open tab keeps
   * trying for as long as it lives, exactly like a native `agent-share`
   * process waiting for peers. The mesh's own healing after its beacon dies
   * (the producer is always the beacon) runs on cadences measured in
   * minutes, so any budget short of that concluded "the share is gone" about
   * a share that was seconds from coming back — measured: a 90 s budget
   * expired mid-ladder and the very attempt it aborted then landed. The
   * manifest stays on screen and navigable the whole time; only the two
   * actions that reach the peer wait.
   */
  function ensureLive(): Promise<void> {
    if (ctx.aborted.aborted) return Promise.resolve()
    if (revivalInFlight) return revivalInFlight
    const current = state.peek()
    if (current.phase !== 'ready' || !current.client.closed) return Promise.resolve()

    revivalInFlight = (async () => {
      // The `ready` state is deliberately left in place: the manifest is still
      // good, so the browser stays on screen and navigable while this runs.
      // Only `revival` flips, and only the actions that need the peer read it.
      revival.value = { attempts: 0, lastError: null }
      let backoff = RECONNECT_BACKOFF_START_MS
      let attempt = 0
      // The dying client, kept ALIVE until its replacement is up. Its mount
      // connection is gone but its serving half is not: the mesh membership,
      // the published card, and the store-backed mount handler all still
      // answer. Tearing it down first — what `release` did here — meant every
      // seeder of a dead-origin share left the mesh at the same instant, so
      // the reconnect had nobody to fall back to. Kept, a reviving tab can
      // bootstrap from the *other* tab's retiring client, or even its own
      // (a different endpoint id that happens to hold the bytes).
      //
      // Retired only on a successful swap: while attempts run — however long
      // that takes — the bytes this tab holds keep serving, which is what
      // lets its peers (and eventually its own replacement) find the share.
      const retiring = pending
      let evicted = false
      try {
        while (!ctx.aborted.aborted) {
          try {
            if (!evicted) {
              evicted = evict(props.ticket, props.transport, retiring)
            }
            release(props.ticket, props.transport, pending)
            // Each attempt is self-terminating (the wasm side caps the origin
            // dial and bounds its card wait), so no outer race is needed —
            // control always comes back here to try again.
            pending = connect(
              props.ticket,
              props.transport,
              revivalOriginCapMs(attempt),
              password,
            )
            await bringUp(pending)
            // The swap point: the replacement is up (and re-seeding via
            // `refresh_held`), so the old client may finally say goodbye.
            retire(retiring)
            return
          } catch (error) {
            if (ctx.aborted.aborted) return
            console.debug('[agent-share] reconnect attempt failed; retrying', error)
            attempt += 1
            revival.value = { attempts: attempt, lastError: String(error) }
            await new Promise((resolve) => setTimeout(resolve, jittered(backoff)))
            backoff = Math.min(backoff * 2, RECONNECT_BACKOFF_MAX_MS)
          }
        }
      } finally {
        revival.value = null
        revivalInFlight = null
      }
    })()
    return revivalInFlight
  }

  const RECONNECT_BACKOFF_START_MS = 1_000
  /**
   * Steady-state retry ceiling. Attempts run forever, and each one costs a
   * mesh identity on the wasm side only until the waiting membership is
   * established — after that, retries reuse it. With the wasm side's card
   * wait cut to ~12 s, a 30 s gap between attempts would dominate the
   * ladder; 10 s keeps the duty cycle near half while an hour of dead
   * origin still costs only polite, membership-reusing attempts.
   */
  const RECONNECT_BACKOFF_MAX_MS = 10_000
  /**
   * How far the clock may move between two 1 s ticks before it counts as a
   * sleep. Well above what a hidden tab's throttled timer does (13-20 s,
   * measured in Safari) and well below the shortest sleep anyone notices.
   */
  const SLEEP_GAP_MS = 60_000

  /**
   * Notice a connection that died while nothing was using it.
   *
   * `closed` is a plain getter on the wasm client, not a signal, and nothing
   * pushes on close — `watch`'s follower just returns. Polling is the cheap way
   * in and matches how this app already samples the client for stats
   * (`TechInfo`). A push callback from wasm would be tidier and can replace
   * this without touching callers.
   *
   * Guarded on `ready`: while connecting or failed there is nothing to revive,
   * and without the guard a share whose producer is gone would spin.
   */
  let lastTickAt = Date.now()
  using _liveness = interval(1000, () => {
    const now = Date.now()
    const slept = now - lastTickAt > SLEEP_GAP_MS
    lastTickAt = now
    const current = state.peek()
    if (current.phase !== 'ready') return
    // A jump in the clock between ticks is the machine having slept. The
    // connection may not report `closed` yet — QUIC only learns of the idle
    // timeout on its next send — but it is dead all the same, so start early
    // rather than wait a tick for the flag.
    if (current.client.closed || slept) {
      if (slept) console.debug('[agent-share] clock jumped; the machine slept')
      void ensureLive()
    }
  })

  // The tab coming back into view, and the network coming back at all. A
  // wake with this tab already in front fires neither `visibilitychange`
  // nor, on a wired machine, `online`; the poll above covers that case.
  const onVisible = () => {
    if (document.visibilityState === 'visible') void ensureLive()
  }
  const onOnline = () => void ensureLive()
  document.addEventListener('visibilitychange', onVisible)
  window.addEventListener('online', onOnline)
  ctx.aborted.addEventListener('abort', () => {
    document.removeEventListener('visibilitychange', onVisible)
    window.removeEventListener('online', onOnline)
  })

  // Drop the mount when the session unmounts (ticket change / leave).
  ctx.aborted.addEventListener('abort', () => {
    void clearMount()
  })

  const tree = computed(() => {
    const current = state.value
    return current.phase === 'ready' ? treeOf(current.manifest) : null
  })

  async function downloadFiles(files: FileNode[], baseName: string): Promise<void> {
    if (transfer.peek() || files.length === 0) return
    // Re-dial first if the tab was away long enough to lose the connection.
    // Without this the first click after coming back always failed, and the
    // failure named an internal stream operation rather than the cause.
    await ensureLive()
    const current = state.peek()
    if (current.phase !== 'ready') return
    // One file travels as itself; only a multi-file selection needs a ZIP.
    const single = files.length === 1 ? files[0] : null

    // The destination is chosen before anything else exists. Dismissing the
    // dialog then costs nothing to unwind: no progress bar went up over a
    // transfer that had nowhere to go, and no read was opened against the peer
    // — a `ReadableStream` pulls the moment it is constructed.
    let target: SaveTarget
    try {
      target = await pickSaveTarget(single ? single.name : `${baseName}.zip`)
    } catch (error) {
      // User dismissed the picker — not an error worth surfacing.
      if (error instanceof DOMException && error.name === 'AbortError') return
      reportFailure(error)
      return
    }

    const abort = new AbortController()
    transfer.value = {
      kind: 'download',
      progress: {
        done: 0,
        total: files.reduce((sum, file) => sum + file.size, 0),
      },
      abort,
    }
    const untrack = trackHoldings(current.client)
    try {
      const onProgress = (progress: Progress) => {
        transfer.value = { kind: 'download', progress, abort }
      }
      const stream = single
        ? singleFileStream(current.client, single, onProgress, abort.signal)
        : zipStream(current.client, files, onProgress, abort.signal)
      await target.write(stream, abort.signal)
    } catch (error) {
      // Cancelling is a decision, not a failure. Anything else is reported on
      // the page rather than rethrown: every caller `void`s this, so a
      // rethrow became an unhandled rejection — the user saw a crash overlay
      // naming an internal operation, and the actual cause (producer gone, or
      // a connection expired while the tab was backgrounded) reached nobody.
      const cancelled =
        abort.signal.aborted || (error instanceof DOMException && error.name === 'AbortError')
      if (!cancelled) reportFailure(error)
    } finally {
      untrack()
      if (transfer.peek()?.kind === 'download') transfer.value = null
      // Whatever landed — including a cancelled transfer's whole chunks — is
      // now servable, so adopt it into the seeder and say so. After the
      // `finally`, deliberately: a download that failed still leaves real
      // chunks behind, and refusing to seed them would throw away the one
      // thing a partial transfer is still good for.
      publishHoldings(current.client)
    }
  }

  /**
   * Adopt what this tab now holds into the seeder, then advertise it.
   *
   * Serving before advertising is the client's own ordering rule; this just
   * asks for it and repaints. Failures are logged, never surfaced: a tab that
   * cannot seed is still a tab that downloaded its file.
   */
  function publishHoldings(client: Client): void {
    void client
      .republish_holdings()
      .then(() => {
        refreshHeld(client)
      })
      .catch((error: unknown) => {
        console.debug('[share] publishing what we hold failed', error)
      })
  }

  /**
   * Repaint what this tab holds while bytes are still arriving.
   *
   * A file is seedable chunk by chunk, so leaving the grid on "not held" until
   * the transfer ends understates what this tab is already handing the swarm —
   * for the whole window where that is most worth saying. Returns the stop.
   *
   * Polled rather than driven off progress because `sync` reports none of its
   * own, and on an interval because `coverage_map` asks the store once per known
   * row. A tab in the background gets throttled to seconds by the browser, which
   * costs nothing here: the repaint is a courtesy, and the `finally` below is
   * what guarantees the final state.
   */
  function trackHoldings(client: Client): () => void {
    const timer = setInterval(() => refreshHeld(client), HOLDINGS_REPAINT_MS)
    return () => clearInterval(timer)
  }

  /** Take the client's held set into the signal, and repaint. */
  function refreshHeld(client: Client): void {
    held.value = new Set(Array.from(client.held))
    // Coverage is asynchronous — it reads the store — so it lands a tick after
    // the complete-slot set. Fire-and-forget: a tab whose storage is refused
    // still shows everything it holds whole.
    void client
      .coverage_map()
      .then((raw: unknown) => {
        const next = new Map<number, number>()
        for (const [key, value] of Object.entries(raw as Record<string, number>)) {
          const index = Number.parseInt(key, 10)
          if (Number.isInteger(index) && typeof value === 'number') next.set(index, value)
        }
        coverage.value = next
      })
      .catch(() => {
        // Losing the fractions costs the partial shading, nothing else.
      })
  }

  /**
   * Fetch bytes so this tab can seed them.
   *
   * `only` is a path filter — a file or a folder — or nothing for the whole
   * share. Already-held files are skipped by the client, so pressing this
   * twice is cheap rather than a re-download.
   */
  async function seedShare(only?: string[]): Promise<void> {
    if (seeding.peek()) return
    // Re-dial first, for the same reason `downloadFiles` does: seeding pulls
    // the bytes over the mount connection, so a tab that was backgrounded long
    // enough to lose it would fail here and blame storage.
    await ensureLive()
    const current = state.peek()
    if (current.phase !== 'ready') return
    seeding.value = true
    const untrack = trackHoldings(current.client)
    try {
      await current.client.sync(only)
      refreshHeld(current.client)
    } catch (error) {
      // Storage can be refused outright — private mode, or a full quota — and
      // that must cost seeding rather than the share. Surfaced rather than
      // logged: a Seed button that silently does nothing is worse than one
      // that says why.
      reportFailure(error)
    } finally {
      untrack()
      seeding.value = false
      // A sync that failed part-way still left whole chunks behind, and they
      // are seedable — so the last word on what this tab holds comes after the
      // failure, not only after a success.
      refreshHeld(current.client)
    }
  }

  async function seedSelected(): Promise<void> {
    const built = tree.peek()
    if (!built) return
    const selected = nodeAtPath(built.root, path.peek())
    if (!selected) return
    await seedShare([selected.path])
  }

  async function downloadSelected(): Promise<void> {
    const built = tree.peek()
    if (!built) return
    const selected = nodeAtPath(built.root, path.peek())
    if (!selected) return
    await downloadFiles(filesUnder(selected), selected.name || 'share')
  }

  async function downloadAll(): Promise<void> {
    const built = tree.peek()
    if (!built) return
    // Named for when it was taken, in the same shape as the mount folder the
    // CLI and this app both create — so a share's artifacts sort together, and
    // two downloads are told apart without opening either.
    await downloadFiles(filesUnder(built.root), shareStamp())
  }

  async function mount(): Promise<void> {
    if (mountSession.peek()) {
      mountError.value = null
      await clearMount()
      return
    }
    // No `canMount()` guard: the button is disabled when it returns false, so
    // this is unreachable without one — and reporting it after the click was
    // exactly the thing worth fixing.
    if (transfer.peek()) return
    // Closes the gap between a connection dying and the poll noticing: a click
    // landing in that second would otherwise mirror the whole file tree over a
    // connection that is already gone.
    await ensureLive()
    if (state.peek().phase !== 'ready') return
    mountError.value = null
    try {
      const session = await pickMountRoot()
      if (ctx.aborted.aborted) {
        await disposeMount(session)
        return
      }
      mountSession.value = session
      synced = emptySyncedState()
      await runSync('mounting')
    } catch (error) {
      // User dismissed the picker — not an error worth surfacing.
      if (error instanceof DOMException && error.name === 'AbortError') return
      reportMountFailure(error)
      await clearMount()
    }
  }

  // Identity-stable handlers for ColumnView. A page's thunk re-runs once per
  // transfer chunk (it reads `transfer.value`), and ColumnView's memo can only
  // bail when every prop it reads keeps its identity — an inline arrow re-made
  // per run forces the whole file tree to re-reconcile at chunk rate. See
  // `ColumnView`'s own test, which pins both shapes.
  const onPathChange = (next: string[]) => {
    path.value = next
  }
  const onDownload = () => void downloadSelected()
  const onSeed = () => void seedSelected()
  const onPreview = () => {
    const selected = path.peek()
    if (selected.length > 0) nav.go(props.ticket, 'preview', { file: selected })
  }
  const noop = () => undefined

  const ready = computed<SessionReady | null>(() => {
    const current = state.value
    const built = tree.value
    if (current.phase !== 'ready' || !built) return null
    return { client: current.client, root: built.root, skipped: built.skipped }
  })
  const mounted = computed(() => mountSession.value !== null)

  const api: SessionApi = {
    ticket: props.ticket,
    ready,
    path,
    held,
    coverage,
    transfer,
    sample,
    openedAt,
    lastActivityAt,
    tick,
    seeding,
    toast,
    mountError,
    redialling: computed(() => revival.value !== null),
    revival,
    mounted,
    status: computed(() => {
      const active = transfer.value
      return revival.value
        ? 'reconnecting'
        : active
          ? transferLabel(active.kind)
          : mounted.value
            ? 'mounted'
            : 'ready'
    }),
    wantsPeerIps,
    dismissToast,
    downloadAll: () => void downloadAll(),
    seedShare: () => void seedShare(),
    mount: () => void mount(),
    publishHoldings: () => {
      const current = state.peek()
      if (current.phase === 'ready') publishHoldings(current.client)
    },
    onPathChange,
    onDownload,
    onSeed,
    onPreview,
  }
  // Before the first yield: context only reaches children mounted after it,
  // and the outlet below is one of them.
  this.provide(SessionCtx, api)

  yield () => {
    const current = state.value
    if (current.phase === 'connecting') {
      const offline = offlineManifest.value
      if (offline) {
        const offlineBuilt = treeOf(offline)
        // The tab's own copy, browsable while the dial grinds. The two
        // actions that reach a peer stay disabled; everything else is local.
        // The only message this phase can raise is the hidden-entries one, and
        // it comes from the very manifest being drawn.
        const message = toast.value
        return (
          <Chrome
            crumb="connecting"
            toast={message ? <Toast {...message} onClose={dismissToast} /> : null}
          >
            <ColumnView
              root={offlineBuilt.root}
              path={path.value}
              onPathChange={onPathChange}
              onDownload={noop}
              downloadDisabled
              held={held.value}
              coverage={coverage.value}
              onSeed={noop}
              seedDisabled
              onPreview={noop}
              previewDisabled
            />
          </Chrome>
        )
      }
      return (
        <Chrome>
          <LoadingBody label="connecting…" />
        </Chrome>
      )
    }
    if (current.phase === 'needs-password') {
      return (
        <Chrome crumb="locked">
          <PasswordGate error={current.error} onSubmit={submitPassword} />
        </Chrome>
      )
    }
    if (current.phase === 'failed') {
      return (
        <Chrome crumb="failed">
          <FailedBody reason={current.reason} />
        </Chrome>
      )
    }
    if (current.phase !== 'ready') return null
    // The page for whichever share view the URL names. It reads everything it
    // needs off `SessionCtx`.
    return Outlet()
  }
})

/**
 * The layout route under `/`, and the only place `Session` is mounted.
 *
 * The key is the whole point: the router keeps a depth-0 route component alive
 * across every child navigation, which is what makes `/files` ↔ `/info` free —
 * and would also make a *ticket* change free, silently reusing a client dialled
 * for a different share. Keying on ticket + transport puts the remount back.
 */
export const SessionLayout = component(function* () {
  const params = useParams(this)
  const location = useLocation(this)

  yield () => {
    const ticket = params.value['ticket'] ?? ''
    const transport = parseTransport(location.value.search)
    return (
      <Session key={clientKey(ticket, transport)} ticket={ticket} transport={transport} />
    )
  }
})
