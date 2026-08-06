/**
 * The share browser.
 *
 * Share routes put the ticket in the path so views are shareable:
 * `/files/<ticket>` for the file browser and `/info/<ticket>` for session info.
 */

import {
  Badge,
  Box,
  Button,
  ProgressBar,
  Spinner,
  Stack,
  Text,
  roleVar,
} from 'moonspace-ui'
import { component, computed, interval, signal } from 'visage-dom'
import type { Child, Ctx } from 'visage-dom'

import { ColumnView } from './ColumnView.tsx'
import { seedState } from './seeding.ts'
import { TechInfo } from './TechInfo.tsx'
import { TransferStatus } from './TransferStatus.tsx'
import type { LinkSample, TransferSnapshot } from './transferStats.ts'
import {
  pickSaveTarget,
  singleFileStream,
  zipStream,
  type Progress,
  type SaveTarget,
} from './download.ts'
import {
  canMount,
  disposeMount,
  emptySyncedState,
  MountError,
  pickMountRoot,
  shareStamp,
  syncMount,
  type MountSession,
  type SyncedState,
} from './mount.ts'
import { canProduce, pickShareRoot, startProducer, type ShareProducer } from './produce.ts'
import { buildPeerCard } from './peerCard/index.ts'
import {
  navigateToShare,
  onRouteChange,
  parseRoute,
  parseShareInput,
  shareUrl,
  type ShareView,
  type TransportMode,
} from './ticket/index.ts'
import {
  buildTree,
  filesUnder,
  humanBytes,
  nodeAtPath,
  type FileNode,
  type Manifest,
} from './tree.ts'
import { loadWasm } from './wasm.ts'

interface Client {
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
  /** Members on the share's mesh, including us. 0 when the mesh is not up. */
  readonly peers_gossip: number
  /** Peers we hold a direct WebRTC data channel with. */
  readonly peers_direct: number
  readonly max_direct: number
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

type State =
  | { phase: 'idle' }
  | { phase: 'connecting' }
  | { phase: 'ready'; client: Client; manifest: Manifest }
  | { phase: 'failed'; reason: string; kind?: FailureKind }

type HomeState =
  | { phase: 'landing' }
  | { phase: 'creating' }
  | { phase: 'serving'; producer: ShareProducer }
  | { phase: 'failed'; reason: string; kind?: FailureKind }

/**
 * `unsupported` is a missing browser capability, not a connection problem —
 * retrying or opening a firewall will never help, so it gets its own copy.
 */
type FailureKind = 'unsupported'

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
function clientKey(ticket: string, transport?: TransportMode): string {
  return `${transport ?? 'dynamic'} ${ticket}`
}

function connect(
  ticket: string,
  transport?: TransportMode,
  originCapMs?: number,
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
 * joins the share's mesh *before* it resolves, so a session abandoned while
 * still connecting already has a live membership broadcasting heartbeats — and
 * the silence sweeper will never evict it, because it is not silent. Every
 * other viewer of that share counts a member with no UI behind it, forever.
 * Changing the hash mid-connect was enough to leave one.
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
function release(ticket: string, transport: TransportMode | undefined, owned: Promise<Client>): void {
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
function evict(ticket: string, transport: TransportMode | undefined, owned: Promise<Client>): boolean {
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
function retire(owned: Promise<Client>): void {
  void owned
    .then((client) => client.leave_mesh())
    .catch((error: unknown) => {
      console.debug('[agent-share] leaving the mesh failed on teardown', error)
    })
}

/** Fall back to the deepest prefix that still exists in the manifest. */
function prunePath(current: string[], manifest: Manifest): string[] {
  const dirs = new Set(manifest.dirs.map((dir) => dir.rel_path))
  let depth = current.length
  while (depth > 0 && !dirs.has(current.slice(0, depth).join('/'))) depth -= 1
  return depth === current.length ? current : current.slice(0, depth)
}

/**
 * App chrome: top bar on the sunken page background + content surface on `bg`
 * (same split as the file browser). `belowBar` is optional status under the
 * main top-bar line.
 */
function SessionChrome({
  crumb,
  center,
  trailing,
  belowBar,
  children,
}: {
  /** When omitted, the top bar is just the brand. */
  crumb?: string
  /** Status between the brand and the actions. Must be exactly `oneRow` tall. */
  center?: Child
  trailing?: Child
  belowBar?: Child
  children: Child
}) {
  return (
    <div
      style={{
        display: 'flex',
        flexDirection: 'column',
        height: '100vh',
        minHeight: 0,
      }}
    >
      <div
        style={{
          flexShrink: 0,
          padding: 'var(--ms-row) 2ch',
          display: 'flex',
          flexDirection: 'column',
          gap: 'calc(1 * var(--ms-row))',
        }}
      >
        {/*
          A grid, not a flex row, and only because of the middle slot.

          Centring the status in the *slack* between the two ends ties its
          position to their widths, and both ends change width on their own
          schedule — the trailing end swaps its whole button row for a progress
          bar and `Cancel` mid-transfer, and relabels `Seed` / `Seeding` and
          `Mount` / `Unmount` besides. Measured, that dragged the whole readout
          23px sideways mid-transfer, which is precisely the jitter the
          readout's own fixed-width fields exist to prevent, arriving one level
          up.

          Equal `minmax(0, 1fr)` rails on either side of an `auto` middle pin
          the middle to the *container's* centre instead, so it holds still
          whatever the ends do. `minmax(0, …)` rather than `1fr` so a rail may
          shrink below its content on a narrow window; the middle is the one
          thing that must not move.
        */}
        <div
          style={{
            display: 'grid',
            // `minmax(0, auto)` for the middle, not plain `auto`: an `auto`
            // track refuses to shrink below its content, so on a narrow window
            // the actions overflow *into* the status and the two draw on top of
            // each other. This lets the status be the one that gives.
            gridTemplateColumns: center
              ? 'minmax(0, 1fr) minmax(0, auto) minmax(0, 1fr)'
              : '1fr auto',
            alignItems: 'center',
            gap: '2ch',
          }}
        >
          <Stack direction="row" gap={1}>
            <Text weight="bold">agent-share</Text>
            {crumb ? (
              <>
                <Text color="fgMuted">/</Text>
                <Text color="fgMuted">{crumb}</Text>
              </>
            ) : null}
          </Stack>
          {center ? (
            // `overflow: hidden` so a window too narrow for everything clips
            // the status rather than shoving the actions off the edge.
            <div style={{ display: 'flex', justifyContent: 'center', overflow: 'hidden' }}>
              {center}
            </div>
          ) : null}
          <div style={{ display: 'flex', justifyContent: 'flex-end', minWidth: 0 }}>
            {trailing ?? null}
          </div>
        </div>
        {belowBar ?? null}
      </div>
      <div
        style={{
          flex: 1,
          minHeight: 0,
          display: 'flex',
          flexDirection: 'column',
          background: roleVar.bg,
        }}
      >
        {children}
      </div>
    </div>
  )
}

function Centered({ children }: { children: Child }) {
  return (
    <div
      style={{
        flex: 1,
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
        minHeight: 0,
        width: '100%',
      }}
    >
      {children}
    </div>
  )
}

function LoadingBody({ label }: { label: string }) {
  return (
    <Centered>
      <Stack direction="row" gap={1}>
        <Spinner />
        <Text>{label}</Text>
      </Stack>
    </Centered>
  )
}

function FailedBody({ reason, kind }: { reason: string; kind?: FailureKind }) {
  const unsupported = kind === 'unsupported'
  const iceHint =
    !unsupported &&
    /ice_connection_state|ondatachannel|ICE failed|no ICE candidates/i.test(reason)
  return (
    <Centered>
      <div style={{ padding: '0 2ch', maxWidth: '60ch' }}>
        <Box border="line" padX={2} padY={1}>
          <Stack direction="column" gap={1}>
            <Text weight="bold" color="danger">
              {unsupported ? 'Not supported in this browser' : 'Could not connect'}
            </Text>
            <Text color="fgMuted">
              {unsupported
                ? 'Sharing a folder needs the File System Access API, which Safari and Firefox do not implement. Receiving a share works here; to send one, use Chrome, Edge, or another Chromium browser.'
                : iceHint
                  ? 'WebRTC could not open a path between the two browsers (LAN/mDNS and TURN both failed). On macOS, allow Local Network for this browser under System Settings → Privacy & Security → Local Network, hard-refresh both tabs, and retry.'
                  : 'A direct connection to this peer could not be established. Both ends may be behind restrictive NATs.'}
            </Text>
            {/* The raw error, which is exactly what gets pasted into a report. */}
            <Text color="fgSubtle" class="selectable">
              {reason}
            </Text>
          </Stack>
        </Box>
      </div>
    </Centered>
  )
}

const Home = component(function* (_props, ctx: Ctx) {
  const state = signal<HomeState>({ phase: 'landing' })

  async function createShare(): Promise<void> {
    if (!canProduce()) {
      state.value = {
        phase: 'failed',
        kind: 'unsupported',
        reason: 'This browser cannot share folders (File System Access API required)',
      }
      return
    }
    try {
      const root = await pickShareRoot()
      if (ctx.aborted.aborted) return
      state.value = { phase: 'creating' }
      const producer = await startProducer(root)
      if (ctx.aborted.aborted) {
        await producer.stop()
        return
      }
      state.value = { phase: 'serving', producer }
    } catch (error) {
      if (error instanceof DOMException && error.name === 'AbortError') return
      if (!ctx.aborted.aborted) {
        state.value = { phase: 'failed', reason: String(error) }
      }
    }
  }

  function joinShare(): void {
    const raw = window.prompt('Paste a share ticket or URL')
    if (raw === null) return
    const ticket = parseShareInput(raw)
    if (!ticket) return
    navigateToShare(ticket, 'files')
  }

  async function stopServing(): Promise<void> {
    const current = state.peek()
    if (current.phase !== 'serving') return
    // Leave 'serving' before awaiting, not after. The teardown takes a mesh
    // departure broadcast and an endpoint close; while that runs the button is
    // still on screen, and a second click used to re-read 'serving' and call
    // `stop()` again.
    state.value = { phase: 'landing' }
    try {
      await current.producer.stop()
    } catch (error) {
      // Nothing left to recover: the share is already off the UI. Report it
      // rather than surfacing an unhandled rejection.
      console.warn('[share] stopping the share failed', error)
    }
  }

  ctx.aborted.addEventListener('abort', () => {
    const current = state.peek()
    if (current.phase === 'serving') void current.producer.stop()
  })

  yield () => {
    const current = state.value
    if (current.phase === 'creating') {
      return (
        <SessionChrome>
          <LoadingBody label="creating share…" />
        </SessionChrome>
      )
    }
    if (current.phase === 'failed') {
      return (
        <SessionChrome
          trailing={
            <Button variant="secondary" onclick={() => {
              state.value = { phase: 'landing' }
            }}>
              Back
            </Button>
          }
        >
          <FailedBody reason={current.reason} kind={current.kind} />
        </SessionChrome>
      )
    }
    if (current.phase === 'serving') {
      const url = shareUrl(current.producer.ticket)
      return (
        <SessionChrome
          trailing={
            <Button variant="danger" onclick={() => void stopServing()}>
              Stop sharing
            </Button>
          }
        >
          <Centered>
            <div style={{ padding: '0 2ch', maxWidth: '72ch', width: '100%' }}>
              <Box border="line" padX={2} padY={1}>
                <Stack direction="column" gap={1}>
                  <Stack direction="row" gap={1}>
                    <Text weight="bold">Sharing</Text>
                    <Badge tone="success" variant="outline">
                      {current.producer.transport}
                    </Badge>
                    <Text color="fgMuted">
                      {current.producer.files} files · {humanBytes(current.producer.bytes)}
                    </Text>
                  </Stack>
                  <Text color="fgMuted">Peers open this link:</Text>
                  {/*
                    `Copy link` below takes the whole string; selection is for
                    taking part of it — the ticket alone, say.
                  */}
                  <Text class="selectable">{url}</Text>
                  <Button
                    variant="primary"
                    onclick={() => {
                      void navigator.clipboard.writeText(url)
                    }}
                  >
                    Copy link
                  </Button>
                </Stack>
              </Box>
            </div>
          </Centered>
        </SessionChrome>
      )
    }

    return (
      <SessionChrome>
        <Centered>
          <Stack direction="column" gap={1}>
            <Button variant="primary" onclick={() => void createShare()}>
              Add files/folder
            </Button>
            <Button variant="secondary" onclick={() => joinShare()}>
              Join a share
            </Button>
          </Stack>
        </Centered>
      </SessionChrome>
    )
  }
})

/**
 * One dialled session for a fixed ticket. Remounted (via `key`) when the
 * ticket changes so the previous watch/dial is disposed through ctx.aborted.
 * The files/info view is a prop driven by the path — switching views does not
 * remount the session.
 */
type Transfer = {
  kind: 'download' | 'mounting' | 'syncing'
  progress: Progress
  abort: AbortController
}

/** What the top bar says while `kind` is in flight. */
function transferLabel(kind: Transfer['kind']): string {
  return kind === 'download' ? 'downloading' : kind === 'mounting' ? 'mounting' : 'syncing'
}

const Session = component<{
  ticket: string
  view: ShareView
  transport?: TransportMode
  dev?: boolean
}>(function* (
  props,
  ctx: Ctx,
) {
  const state = signal<State>({ phase: 'connecting' })
  const path = signal<string[]>([])
  const transfer = signal<Transfer | null>(null)
  const mountError = signal<string | null>(null)
  /**
   * Why the last download stopped, when it was not a cancellation.
   *
   * A transfer can fail for reasons the peer connection knows about and the
   * page cannot guess — the producer stopped sharing, or the connection
   * expired while the tab sat in the background. Those need to reach the user
   * as text on the page; before this they reached them as an unhandled
   * rejection, which reads as a crash.
   */
  const downloadError = signal<string | null>(null)
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
  const reviving = signal(false)
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
  /** Whether a seed is in flight, so the button can say it is busy. */
  const seeding = signal(false)
  const seedError = signal<string | null>(null)
  /**
   * The latest transfer reading, from the one sampler below.
   *
   * A signal rather than a prop computed during render, so only the readouts
   * that read it repaint each second — the file list must not.
   */
  const sample = signal<TransferSnapshot | null>(null)
  /** Bumped by the same tick, for views that re-read `info()` rather than this. */
  const tick = signal(0)

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
    // getStats, for the per-peer rows and the ICE addresses behind them.
    void current.client.refresh_peer_ips()
    // QUIC, for the whole connection — the half that answers on the relay path,
    // where there is no candidate pair to ask.
    sample.value = {
      link: current.client.sample_link(),
      gossip: current.client.peers_gossip,
      direct: current.client.peers_direct,
      // A live mount on the relay (or IP) is a connected peer the direct
      // count cannot see — the WebRTC-path mount is already inside it.
      relayPeer: !current.client.closed && current.client.transport !== 'webrtc',
    }
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
        const latestTree = buildTree(latest.manifest)
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
        mountError.value = error instanceof MountError ? error.message : String(error)
        await clearMount()
      }
    } finally {
      syncing = false
      if (transfer.peek()?.kind === 'mounting' || transfer.peek()?.kind === 'syncing') {
        transfer.value = null
      }
    }
  }

  // Dial now, and register the release before the first await. A ticket change
  // unmounts this session; the mesh membership belongs to it, so it goes too —
  // and it has to go even when the abort lands *during* the connect, which is
  // why this holds the promise rather than a client we may not have yet.
  let pending = connect(props.ticket, props.transport)
  ctx.aborted.addEventListener('abort', () => release(props.ticket, props.transport, pending))

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
    if (current.phase === 'ready') current.client.leave_mesh()
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
    path.value = prunePath(path.peek(), manifest)
    state.value = { phase: 'ready', client, manifest }
    // What survived a previous visit. Reads storage without creating any, so
    // a tab that only browses leaves nothing behind. Inside `bringUp` rather
    // than beside the first call, so a revived connection re-reads it too —
    // the client is new after a redial and its held set starts empty.
    void client.refresh_held().then(() => {
      if (!ctx.aborted.aborted) refreshHeld(client)
    })
    await client.watch((next) => {
      if (ctx.aborted.aborted) return
      path.value = prunePath(path.peek(), next)
      state.value = { phase: 'ready', client, manifest: next }
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

  void (async () => {
    // Literal backoff bounds: the shared constants are declared later in this
    // body, and this loop starts synchronously — before they initialize.
    let backoff = 1_000
    while (!ctx.aborted.aborted) {
      try {
        await bringUp(pending)
        return
      } catch (error) {
        if (ctx.aborted.aborted) return
        if (permanentConnectError(error)) {
          state.value = { phase: 'failed', reason: String(error) }
          return
        }
        console.debug('[agent-share] connect failed; retrying', error)
        await new Promise((resolve) => setTimeout(resolve, backoff))
        backoff = Math.min(backoff * 2, 30_000)
        release(props.ticket, props.transport, pending)
        pending = connect(props.ticket, props.transport)
      }
    }
  })()

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
      // Only `reviving` flips, and only the actions that need the peer read it.
      reviving.value = true
      let backoff = RECONNECT_BACKOFF_START_MS
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
            pending = connect(props.ticket, props.transport, REVIVAL_ORIGIN_CAP_MS)
            await bringUp(pending)
            // The swap point: the replacement is up (and re-seeding via
            // `refresh_held`), so the old client may finally say goodbye.
            retire(retiring)
            return
          } catch (error) {
            if (ctx.aborted.aborted) return
            console.debug('[agent-share] reconnect attempt failed; retrying', error)
            await new Promise((resolve) => setTimeout(resolve, backoff))
            backoff = Math.min(backoff * 2, RECONNECT_BACKOFF_MAX_MS)
          }
        }
      } finally {
        reviving.value = false
        revivalInFlight = null
      }
    })()
    return revivalInFlight
  }

  /** Origin-dial slice of a revival attempt — see `connect`'s cap note. */
  const REVIVAL_ORIGIN_CAP_MS = 8_000
  const RECONNECT_BACKOFF_START_MS = 1_000
  /**
   * Steady-state retry ceiling. Attempts run forever, and each one costs a
   * mesh identity on the wasm side only until the waiting membership is
   * established — after that, retries reuse it. 30 s keeps an hour of dead
   * origin at ~120 polite attempts while still landing within one backoff of
   * the mesh healing itself.
   */
  const RECONNECT_BACKOFF_MAX_MS = 30_000

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
  using _liveness = interval(1000, () => {
    const current = state.peek()
    if (current.phase === 'ready' && current.client.closed) void ensureLive()
  })

  const onVisible = () => {
    if (document.visibilityState === 'visible') void ensureLive()
  }
  document.addEventListener('visibilitychange', onVisible)
  ctx.aborted.addEventListener('abort', () => {
    document.removeEventListener('visibilitychange', onVisible)
  })

  // Drop the mount when the session unmounts (ticket change / leave).
  ctx.aborted.addEventListener('abort', () => {
    void clearMount()
  })

  const tree = computed(() => {
    const current = state.value
    return current.phase === 'ready' ? buildTree(current.manifest) : null
  })

  async function downloadFiles(files: FileNode[], baseName: string): Promise<void> {
    if (transfer.peek() || files.length === 0) return
    // Re-dial first if the tab was away long enough to lose the connection.
    // Without this the first click after coming back always failed, and the
    // failure named an internal stream operation rather than the cause.
    await ensureLive()
    const current = state.peek()
    if (current.phase !== 'ready') return
    downloadError.value = null
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
      downloadError.value = error instanceof Error ? error.message : String(error)
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
      if (!cancelled) {
        downloadError.value = error instanceof Error ? error.message : String(error)
      }
    } finally {
      if (transfer.peek()?.kind === 'download') transfer.value = null
    }
  }

  /** Take the client's held set into the signal, and repaint. */
  function refreshHeld(client: Client): void {
    held.value = new Set(Array.from(client.held))
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
    seedError.value = null
    try {
      await current.client.sync(only)
      refreshHeld(current.client)
    } catch (error) {
      // Storage can be refused outright — private mode, or a full quota — and
      // that must cost seeding rather than the share. Surfaced rather than
      // logged: a Seed button that silently does nothing is worse than one
      // that says why.
      seedError.value = String(error)
      console.warn('[share] sync failed', error)
    } finally {
      seeding.value = false
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
      mountError.value = error instanceof MountError ? error.message : String(error)
      await clearMount()
    }
  }

  yield () => {
    const current = state.value
    if (current.phase === 'connecting') {
      return (
        <SessionChrome>
          <LoadingBody label="connecting…" />
        </SessionChrome>
      )
    }
    if (current.phase === 'failed') {
      return (
        <SessionChrome crumb="failed">
          <FailedBody reason={current.reason} kind={current.kind} />
        </SessionChrome>
      )
    }
    const built = tree.value
    if (current.phase !== 'ready' || !built) return null

    const files = filesUnder(built.root)
    const total = files.reduce((sum, file) => sum + file.size, 0)
    const active = transfer.value
    const mounted = mountSession.value !== null
    // Re-dialling a connection lost to a background tab. Browsing is unaffected
    // — the tree is local — so only the two actions that reach the peer wait.
    const redialling = reviving.value
    const mountable = canMount() && !redialling
    const showingInfo = props.view === 'info'
    const closeInfo = () => {
      navigateToShare(props.ticket, 'files')
    }
    const openInfo = () => {
      navigateToShare(props.ticket, 'info')
    }
    const mountButton = () => (
      <Button variant="ghost" onclick={() => void mount()} disabled={!mountable}>
        {mounted ? 'Unmount' : 'Mount'}
      </Button>
    )
    const err = mountError.value
    const status = redialling
      ? 'reconnecting'
      : active
        ? transferLabel(active.kind)
        : mounted
          ? 'mounted'
          : 'ready'
    // Never "reconnecting": redialing is the app's permanent background
    // posture — it is always willing to reach more peers — so naming it in
    // the chrome would label the normal state of the world. A transfer whose
    // peer is being re-dialed simply shows its own label until bytes resume.
    const crumb = showingInfo ? 'info' : active ? transferLabel(active.kind) : 'files'
    const infoButton = (
      <Button variant="ghost" onclick={openInfo}>
        Info
      </Button>
    )
    /*
      Whole-share seeding. The label carries the state rather than a separate
      line, because this row is exactly `oneRow` tall and anything taller
      would move every pixel of content under it.

      Pressable exactly when pressing it would do something — so the label is
      that same predicate rather than a state of its own. Counts stay out of
      it: once everything is held there is nothing to act on, and the number
      of files still missing is already in the detail column.
    */
    const idle = !seeding.value && seedState(built.root, held.value) !== 'full'
    const seedButton = (
      <Button variant="ghost" onclick={() => void seedShare()} disabled={!idle}>
        {idle ? 'Seed' : 'Seeding'}
      </Button>
    )

    /*
      One row, always. A transfer takes the middle of the row rather than
      adding one below it — every child here is exactly `oneRow` tall, so
      the content underneath never moves.
    */
    let trailing: Child
    if (showingInfo) {
      trailing = (
        <Button variant="ghost" onclick={closeInfo}>
          Close
        </Button>
      )
    } else if (active) {
      trailing = (
        <>
          <ProgressBar
            fluid
            showValue
            value={
              active.progress.total === 0 ? 0 : active.progress.done / active.progress.total
            }
            label={transferLabel(active.kind)}
          />
          <Button variant="danger" onclick={() => active.abort.abort()}>
            Cancel
          </Button>
        </>
      )
    } else {
      trailing = (
        <Stack direction="row" gap={1}>
          {seedButton}
          {infoButton}
          {mountable ? (
            mountButton()
          ) : (
            /*
              The `title` goes on a wrapper, not on the button: a disabled
              control is an unreliable tooltip host, since browsers suppress
              pointer delivery to it. `inline-flex` keeps the wrapper exactly
              `oneRow` tall — a default `inline` span adds line-box leading
              and would break the invariant this row is built on.
            */
            <span
              title="Mounting needs the File System Access API, which this browser lacks. Use Chrome or Edge — or run `npx agent-share <ticket>` to receive the folder locally."
              style={{ display: 'inline-flex' }}
            >
              {mountButton()}
            </span>
          )}
          <Button
            variant="primary"
            onclick={() => void downloadAll()}
            disabled={redialling}
          >
            Download
          </Button>
        </Stack>
      )
    }

    const downloadErr = downloadError.value
    const belowBar =
      err || downloadErr || built.skipped > 0 ? (
        <>
          {/* All three are diagnostics, so all three stay copyable — see `app.css`. */}
          {err ? (
            <Text color="danger" class="selectable">
              {err}
            </Text>
          ) : null}
          {downloadErr ? (
            <Text color="danger" class="selectable">
              {downloadErr}
            </Text>
          ) : null}
          {built.skipped > 0 ? (
            <Text color="warning" class="selectable">
              {built.skipped} entries hidden — unsafe paths in the peer&apos;s manifest
            </Text>
          ) : null}
        </>
      ) : null

    return (
      <SessionChrome
        crumb={crumb}
        center={
          /*
            Dropped while a transfer runs: that branch already gives the whole
            row to a `ProgressBar`, which answers "what is moving" better than a
            rate does, and two answers competing for one row is how the row stops
            being one row. That holds while re-dialling too — hence `active`
            first — and the breadcrumb covers that case instead.

            Otherwise a revival takes the slot from the readout rather than
            sharing it. The four numbers are sampled off the connection that just
            died, so leaving them up draws a page that looks perfectly healthy
            and cannot move a byte.
          */
          /*
            Always up while the browser is: the numbers are wire truth, and
            000 KB/s against a dead peer is exactly what is happening. The
            old blank-while-redialling treatment made the whole bar vanish,
            which read as a broken page — worse than an honest zero.
          */
          active ? null : <TransferStatus sample={sample} />
        }
        trailing={trailing}
        belowBar={belowBar}
      >
        {showingInfo ? (
          <TechInfo
            client={current.client}
            tick={tick}
            fileCount={files.length}
            totalBytes={total}
            status={status}
            mounted={mounted}
            mountError={err}
            dev={props.dev === true}
            killDisabled={redialling}
            onKillConnection={() => {
              current.client.close_connection()
            }}
            onClose={closeInfo}
          />
        ) : (
          <ColumnView
            root={built.root}
            path={path.value}
            onPathChange={(next) => {
              path.value = next
            }}
            onDownload={() => void downloadSelected()}
            downloadDisabled={active !== null || redialling}
            held={held.value}
            onSeed={() => void seedSelected()}
            seedDisabled={seeding.value || redialling}
          />
        )}
      </SessionChrome>
    )
  }
})

export const App = component(function* (_props, ctx: Ctx) {
  const route = signal(parseRoute())
  const stop = onRouteChange(() => {
    route.value = parseRoute()
  })
  ctx.aborted.addEventListener('abort', stop)

  yield () => {
    const current = route.value
    if (!current) return <Home />
    // The transport is in the key as well as the props: changing it has to
    // remount the session, because a live client cannot switch data paths.
    // `dev` deliberately stays out of the key — toggling a debug pane must not
    // redial the share out from under the tab.
    return (
      <Session
        key={clientKey(current.ticket, current.transport)}
        ticket={current.ticket}
        view={current.view}
        transport={current.transport}
        dev={current.dev}
      />
    )
  }
})
