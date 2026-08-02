/**
 * The share browser.
 *
 * The ticket lives in the URL *fragment*, never the path: it is a bearer
 * capability granting full read access, and a path would send it to the server
 * on every request — into logs, proxies and referrers. A fragment never leaves
 * the browser, which is what lets this be a purely static site.
 */

import {
  Badge,
  Box,
  Button,
  ProgressBar,
  Spinner,
  Stack,
  Text,
} from 'moonspace-ui'
import { component, computed, listen, signal } from 'visage-dom'
import type { Child, Ctx } from 'visage-dom'

import { ColumnView } from './ColumnView.tsx'
import { saveStream, singleFileStream, zipStream, type Progress } from './download.ts'
import {
  canMount,
  disposeMount,
  emptySyncedState,
  MountError,
  pickMountRoot,
  syncMount,
  type MountSession,
  type SyncedState,
} from './mount.ts'
import { canProduce, pickShareRoot, startProducer, type ShareProducer } from './produce.ts'
import { parseShareInput, shareUrl } from './ticket.ts'
import {
  buildTree,
  filesUnder,
  humanBytes,
  nodeAtPath,
  type FileNode,
  type Manifest,
} from './tree.ts'

interface Client {
  readonly transport: string
  /** Announce departure from the share's mesh. Safe to call more than once. */
  leave_mesh(): void
  /** Members on the share's mesh, including us. 0 when the mesh is not up. */
  readonly peers_gossip: number
  /** Peers we hold a direct WebRTC data channel with. */
  readonly peers_direct: number
  readonly max_direct: number
  manifest(): Promise<Manifest>
  read(index: number, offset: bigint, len: number): Promise<Uint8Array>
  /** Subscribe to tree changes. Each call delivers the whole manifest. */
  watch(onManifest: (manifest: Manifest) => void): Promise<void>
}

type State =
  | { phase: 'idle' }
  | { phase: 'connecting' }
  | { phase: 'ready'; client: Client; manifest: Manifest }
  | { phase: 'failed'; reason: string }

type HomeState =
  | { phase: 'landing' }
  | { phase: 'creating' }
  | { phase: 'serving'; producer: ShareProducer }
  | { phase: 'failed'; reason: string }

function importWasm() {
  return import('../../crates/agent-share-wasm-client/dist/web/agent_share_wasm_client.js')
}

type WasmModule = Awaited<ReturnType<typeof importWasm>>

/**
 * The WASM module, instantiated at most once per page.
 *
 * Caching the *promise* is load-bearing, not an optimisation. `__wbg_init`
 * guards itself with `if (wasm !== undefined) return wasm`, but it assigns
 * that module-global only *after* its await — so two overlapping calls both
 * miss the guard and each build a `WebAssembly.Instance`. The two instances
 * then share one JS glue module, whose closure table and `wasm` binding now
 * refer to the second: pointers minted by the first get read against the
 * wrong linear memory, and the page dies in `FnOnce called more than once`,
 * `function signature mismatch` and `memory access out of bounds`.
 *
 * The memo also guards any concurrent callers (not only a remounting host) —
 * two overlapping dials for the same page would hit the same bug.
 */
let wasmModule: Promise<WasmModule> | null = null

function loadWasm(): Promise<WasmModule> {
  if (!wasmModule) {
    wasmModule = importWasm().then(async (module) => {
      await module.default()
      return module
    })
    // A failed load must not poison every later attempt.
    wasmModule.catch(() => {
      wasmModule = null
    })
  }
  return wasmModule
}

/**
 * One client per ticket, shared across session mounts.
 *
 * Same reasoning one level up: a second dial for a ticket already being
 * negotiated would open a *second* WebRTC session for the same share — two ICE
 * runs, two data channels, one of them orphaned with no one left to close it.
 */
const clients = new Map<string, Promise<Client>>()

function connect(ticket: string): Promise<Client> {
  let client = clients.get(ticket)
  if (!client) {
    // Omit transport ⇒ dynamic (WebRTC preferred, iroh relay fallback).
    client = loadWasm().then(
      (wasm) => wasm.ShareClient.connect(ticket) as unknown as Promise<Client>,
    )
    // Evict on failure so a retry (a re-entered hash, say) can dial again.
    client.catch(() => clients.delete(ticket))
    clients.set(ticket, client)
  }
  return client
}

function readHash(): string | null {
  return decodeURIComponent(window.location.hash.replace(/^#/, '')).trim() || null
}

/** Fall back to the deepest prefix that still exists in the manifest. */
function prunePath(current: string[], manifest: Manifest): string[] {
  const dirs = new Set(manifest.dirs.map((dir) => dir.rel_path))
  let depth = current.length
  while (depth > 0 && !dirs.has(current.slice(0, depth).join('/'))) depth -= 1
  return depth === current.length ? current : current.slice(0, depth)
}

/** Shared chrome: header row plus a body that fills the rest of the viewport. */
function AppShell({
  trailing,
  children,
}: {
  trailing?: Child
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
        <Stack direction="row" gap={2} justify="between">
          <Stack direction="row" gap={1}>
            <Text weight="bold">agent-share</Text>
          </Stack>
          {trailing ? <Stack direction="row" gap={1}>{trailing}</Stack> : null}
        </Stack>
      </div>
      <div style={{ flex: 1, minHeight: 0, display: 'flex', flexDirection: 'column' }}>
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

function FailedBody({ reason }: { reason: string }) {
  const iceHint =
    /ice_connection_state|ondatachannel|ICE failed|no ICE candidates/i.test(reason)
  return (
    <Centered>
      <div style={{ padding: '0 2ch', maxWidth: '60ch' }}>
        <Box border="line" padX={2} padY={1}>
          <Stack direction="column" gap={1}>
            <Text weight="bold" color="danger">
              Could not connect
            </Text>
            <Text color="fgMuted">
              {iceHint
                ? 'WebRTC could not open a path between the two browsers (LAN/mDNS and TURN both failed). On macOS, allow Local Network for this browser under System Settings → Privacy & Security → Local Network, hard-refresh both tabs, and retry.'
                : 'A direct connection to this peer could not be established. Both ends may be behind restrictive NATs.'}
            </Text>
            <Text color="fgSubtle">{reason}</Text>
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
    window.location.hash = encodeURIComponent(ticket)
  }

  async function stopServing(): Promise<void> {
    const current = state.peek()
    if (current.phase !== 'serving') return
    try {
      await current.producer.stop()
    } finally {
      if (!ctx.aborted.aborted) state.value = { phase: 'landing' }
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
        <AppShell>
          <LoadingBody label="creating share…" />
        </AppShell>
      )
    }
    if (current.phase === 'failed') {
      return (
        <AppShell
          trailing={
            <Button variant="secondary" onclick={() => {
              state.value = { phase: 'landing' }
            }}>
              Back
            </Button>
          }
        >
          <FailedBody reason={current.reason} />
        </AppShell>
      )
    }
    if (current.phase === 'serving') {
      const url = shareUrl(current.producer.ticket)
      return (
        <AppShell
          trailing={
            <Button variant="danger" onclick={() => void stopServing()}>
              Stop sharing
            </Button>
          }
        >
          <Centered>
            <div style={{ padding: '0 2ch', maxWidth: '72ch', width: '100%' }}>
              <Box border="line" background="bg" padX={2} padY={1}>
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
                  <Text>{url}</Text>
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
        </AppShell>
      )
    }

    return (
      <AppShell>
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
      </AppShell>
    )
  }
})

/**
 * One dialled session for a fixed ticket. Remounted (via `key`) when the
 * fragment changes so the previous watch/dial is disposed through ctx.aborted.
 */
type Transfer =
  // Only a download is cancellable — a half-written mount would leave the
  // host folder in a state the next sync has no record of.
  | { kind: 'download'; progress: Progress; abort: AbortController }
  | { kind: 'mounting'; progress: Progress }
  | { kind: 'syncing'; progress: Progress }

/** What the top bar says while `kind` is in flight. */
function transferLabel(kind: Transfer['kind']): string {
  return kind === 'download' ? 'downloading' : kind === 'mounting' ? 'mounting' : 'syncing'
}

const Session = component<{ ticket: string }>(function* (props, ctx: Ctx) {
  const state = signal<State>({ phase: 'connecting' })
  // The peer counts are lock-free reads on the wasm side (an atomic the mesh
  // event loop stores into, and a map length on the transport), so a timer is
  // cheaper than plumbing an event channel out through wasm-bindgen. This
  // signal exists only to make the header recompute; the values are read live.
  const peerTick = signal(0)
  const peerTimer = window.setInterval(() => {
    peerTick.value = peerTick.peek() + 1
  }, 1000)
  ctx.aborted.addEventListener('abort', () => window.clearInterval(peerTimer))
  const path = signal<string[]>([])
  const transfer = signal<Transfer | null>(null)
  const mountError = signal<string | null>(null)
  /** Non-null while a host directory is mounted for this session. */
  const mountSession = signal<MountSession | null>(null)

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
    try {
      do {
        syncDirty = false
        // Re-read the latest ready state each pass — a watch may have landed
        // while the previous write was in flight.
        const latest = state.peek()
        if (latest.phase !== 'ready' || mountSession.peek() !== session) break
        const latestTree = buildTree(latest.manifest)
        const latestFiles = filesUnder(latestTree.root)
        transfer.value = { kind: label, progress: { done: 0, total: 0 } }
        synced = await syncMount(
          session.root,
          latest.client,
          latestFiles,
          latest.manifest.dirs,
          synced,
          (progress) => {
            transfer.value = { kind: label, progress }
          },
        )
        // After the first full mirror, later passes are incremental syncs.
        label = 'syncing'
      } while (syncDirty && mountSession.peek() === session && !ctx.aborted.aborted)
    } catch (error) {
      if (!ctx.aborted.aborted) {
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

  void (async () => {
    try {
      const client = await connect(props.ticket)
      if (ctx.aborted.aborted) return
      const manifest = await client.manifest()
      if (ctx.aborted.aborted) return
      path.value = prunePath(path.peek(), manifest)
      state.value = { phase: 'ready', client, manifest }
      // Announce departure while the page still exists. Without this the tab
      // lingers on every peer's roster until the silence sweeper evicts it —
      // which showed up immediately in testing as a share reporting more
      // members than there were processes.
      //
      // `pagehide`, not `beforeunload`: the latter is unreliable on mobile and
      // is skipped entirely on the bfcache path.
      const onHide = () => client.leave_mesh()
      window.addEventListener('pagehide', onHide)
      ctx.aborted.addEventListener('abort', () => {
        window.removeEventListener('pagehide', onHide)
        // A ticket change unmounts this session; the mesh membership belongs to
        // it, so it goes too. Otherwise navigating between shares accumulates
        // ghosts exactly the way a closed tab did.
        client.leave_mesh()
      })
      await client.watch((next) => {
        if (ctx.aborted.aborted) return
        path.value = prunePath(path.peek(), next)
        state.value = { phase: 'ready', client, manifest: next }
        if (mountSession.peek()) void runSync('syncing')
      })
    } catch (error) {
      if (!ctx.aborted.aborted) state.value = { phase: 'failed', reason: String(error) }
    }
  })()

  // Drop the mount when the session unmounts (ticket change / leave).
  ctx.aborted.addEventListener('abort', () => {
    void clearMount()
  })

  const tree = computed(() => {
    const current = state.value
    return current.phase === 'ready' ? buildTree(current.manifest) : null
  })

  async function downloadFiles(files: FileNode[], baseName: string): Promise<void> {
    const current = state.peek()
    if (current.phase !== 'ready' || transfer.peek() || files.length === 0) return
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
      // One file travels as itself; only a multi-file selection needs a ZIP.
      const single = files.length === 1 ? files[0] : null
      const stream = single
        ? singleFileStream(current.client, single, onProgress, abort.signal)
        : zipStream(current.client, files, onProgress, abort.signal)
      await saveStream(stream, single ? single.name : `${baseName}.zip`, abort.signal)
    } catch (error) {
      // Cancelling is a decision, not a failure — and the callers only ever
      // `void` this, so an unswallowed abort would surface as an unhandled
      // rejection. Anything else still propagates.
      if (!abort.signal.aborted) throw error
    } finally {
      if (transfer.peek()?.kind === 'download') transfer.value = null
    }
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
    await downloadFiles(filesUnder(built.root), 'share')
  }

  async function mount(): Promise<void> {
    if (mountSession.peek()) {
      mountError.value = null
      await clearMount()
      return
    }
    if (!canMount()) {
      mountError.value = 'This browser cannot mount folders'
      return
    }
    if (transfer.peek()) return
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
        <AppShell>
          <LoadingBody label="Connecting…" />
        </AppShell>
      )
    }
    if (current.phase === 'failed') {
      return (
        <AppShell>
          <FailedBody reason={current.reason} />
        </AppShell>
      )
    }
    const built = tree.value
    if (current.phase !== 'ready' || !built) return null

    const files = filesUnder(built.root)
    const total = files.reduce((sum, file) => sum + file.size, 0)
    const active = transfer.value
    const mounted = mountSession.value !== null
    const err = mountError.value
    const hasSelection = nodeAtPath(built.root, path.value) !== undefined
    // Read through the tick so this recomputes each second. `max_direct` is 0
    // exactly when the mesh failed to start, which is also when there is
    // nothing worth showing — so that doubles as the "hide it" signal rather
    // than reporting a misleading `0/0`.
    peerTick.value
    const peers =
      current.client.max_direct === 0
        ? null
        : {
            gossip: current.client.peers_gossip,
            direct: current.client.peers_direct,
            max: current.client.max_direct,
          }

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
            One row, always. A transfer takes the row over rather than adding
            one below it — every child here is exactly `oneRow` tall, so the
            file browser underneath never moves.
          */}
          <Stack direction="row" gap={2} justify="between">
            {active ? (
              <>
                <Text color="fgMuted">{transferLabel(active.kind)}</Text>
                <ProgressBar
                  fluid
                  showValue
                  value={
                    active.progress.total === 0 ? 0 : active.progress.done / active.progress.total
                  }
                  label={transferLabel(active.kind)}
                />
                {active.kind === 'download' ? (
                  <Button variant="danger" onclick={() => active.abort.abort()}>
                    Cancel
                  </Button>
                ) : null}
              </>
            ) : (
              <>
                <Stack direction="row" gap={1}>
                  <Text weight="bold">agent-share</Text>
                  <Badge tone="success" variant="outline">
                    {current.client.transport}
                  </Badge>
                  <Text color="fgMuted">
                    {files.length} files · {humanBytes(total)}
                    {peers === null
                      ? ''
                      : ` · ${peers.direct}/${peers.max} direct · ${peers.gossip} on mesh`}
                  </Text>
                </Stack>
                {/* No `disabled={busy}` needed — this branch only renders when idle. */}
                <Stack direction="row" gap={1}>
                  <Button variant="secondary" onclick={() => void mount()}>
                    {mounted ? 'Unmount' : 'Mount'}
                  </Button>
                  <Button
                    variant="primary"
                    onclick={() => void downloadSelected()}
                    disabled={!hasSelection}
                  >
                    Download
                  </Button>
                  <Button variant="secondary" onclick={() => void downloadAll()}>
                    Download all
                  </Button>
                </Stack>
              </>
            )}
          </Stack>

          {err ? <Text color="danger">{err}</Text> : null}

          {built.skipped > 0 ? (
            <Text color="warning">
              {built.skipped} entries hidden — unsafe paths in the peer&apos;s manifest
            </Text>
          ) : null}
        </div>

        <ColumnView
          root={built.root}
          path={path.value}
          onPathChange={(next) => {
            path.value = next
          }}
        />
      </div>
    )
  }
})

export const App = component(function* () {
  const ticket = signal(readHash())
  using _hash = listen(window, 'hashchange', () => {
    ticket.value = readHash()
  })

  yield () => {
    const current = ticket.value
    if (!current) return <Home />
    return <Session key={current} ticket={current} />
  }
})
