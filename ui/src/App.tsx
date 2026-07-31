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
import { saveZip, zipStream, type Progress } from './download.ts'
import {
  canMount,
  emptySyncedState,
  MountError,
  pickMountRoot,
  syncMount,
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
  | { kind: 'download'; progress: Progress }
  | { kind: 'mounting'; progress: Progress }
  | { kind: 'syncing'; progress: Progress }

const Session = component<{ ticket: string }>(function* (props, ctx: Ctx) {
  const state = signal<State>({ phase: 'connecting' })
  const path = signal<string[]>([])
  const transfer = signal<Transfer | null>(null)
  const mountError = signal<string | null>(null)
  /** Non-null while a host directory is mounted for this session. */
  const mountRoot = signal<FileSystemDirectoryHandle | null>(null)

  let synced: SyncedState = emptySyncedState()
  let syncing = false
  let syncDirty = false

  async function runSync(label: 'mounting' | 'syncing'): Promise<void> {
    const root = mountRoot.peek()
    const current = state.peek()
    if (!root || current.phase !== 'ready') return
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
        if (latest.phase !== 'ready' || mountRoot.peek() !== root) break
        const latestTree = buildTree(latest.manifest)
        const latestFiles = filesUnder(latestTree.root)
        transfer.value = { kind: label, progress: { done: 0, total: 0 } }
        synced = await syncMount(
          root,
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
      } while (syncDirty && mountRoot.peek() === root && !ctx.aborted.aborted)
    } catch (error) {
      if (!ctx.aborted.aborted) {
        mountError.value = error instanceof MountError ? error.message : String(error)
        mountRoot.value = null
        synced = emptySyncedState()
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
      await client.watch((next) => {
        if (ctx.aborted.aborted) return
        path.value = prunePath(path.peek(), next)
        state.value = { phase: 'ready', client, manifest: next }
        if (mountRoot.peek()) void runSync('syncing')
      })
    } catch (error) {
      if (!ctx.aborted.aborted) state.value = { phase: 'failed', reason: String(error) }
    }
  })()

  // Drop the mount when the session unmounts (ticket change / leave).
  ctx.aborted.addEventListener('abort', () => {
    mountRoot.value = null
    synced = emptySyncedState()
  })

  const tree = computed(() => {
    const current = state.value
    return current.phase === 'ready' ? buildTree(current.manifest) : null
  })

  async function downloadFiles(files: FileNode[], suggestedName: string): Promise<void> {
    const current = state.peek()
    if (current.phase !== 'ready' || transfer.peek() || files.length === 0) return
    transfer.value = {
      kind: 'download',
      progress: {
        done: 0,
        total: files.reduce((sum, file) => sum + file.size, 0),
      },
    }
    try {
      const stream = zipStream(current.client, files, (progress) => {
        transfer.value = { kind: 'download', progress }
      })
      await saveZip(stream, suggestedName)
    } finally {
      if (transfer.peek()?.kind === 'download') transfer.value = null
    }
  }

  async function downloadSelected(): Promise<void> {
    const built = tree.peek()
    if (!built) return
    const selected = nodeAtPath(built.root, path.peek())
    if (!selected) return
    const name = selected.kind === 'dir' ? selected.name || 'share' : selected.name
    await downloadFiles(filesUnder(selected), `${name}.zip`)
  }

  async function downloadAll(): Promise<void> {
    const built = tree.peek()
    if (!built) return
    await downloadFiles(filesUnder(built.root), 'share.zip')
  }

  async function mount(): Promise<void> {
    if (mountRoot.peek()) {
      mountRoot.value = null
      synced = emptySyncedState()
      mountError.value = null
      return
    }
    if (!canMount()) {
      mountError.value = 'This browser cannot mount folders'
      return
    }
    if (transfer.peek()) return
    mountError.value = null
    try {
      const root = await pickMountRoot()
      if (ctx.aborted.aborted) return
      mountRoot.value = root
      synced = emptySyncedState()
      await runSync('mounting')
    } catch (error) {
      // User dismissed the picker — not an error worth surfacing.
      if (error instanceof DOMException && error.name === 'AbortError') return
      mountError.value = error instanceof MountError ? error.message : String(error)
      mountRoot.value = null
      synced = emptySyncedState()
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
    const mounted = mountRoot.value !== null
    const busy = active !== null
    const err = mountError.value
    const hasSelection = nodeAtPath(built.root, path.value) !== undefined

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
              <Badge tone="success" variant="outline">
                {current.client.transport}
              </Badge>
              <Text color="fgMuted">
                {files.length} files · {humanBytes(total)}
              </Text>
            </Stack>
            <Stack direction="row" gap={1}>
              <Button variant="secondary" onclick={() => void mount()} disabled={busy}>
                {mounted ? 'Unmount' : 'Mount'}
              </Button>
              <Button
                variant="primary"
                onclick={() => void downloadSelected()}
                disabled={busy || !hasSelection}
              >
                Download
              </Button>
              <Button variant="secondary" onclick={() => void downloadAll()} disabled={busy}>
                Download all
              </Button>
            </Stack>
          </Stack>

          {active ? (
            <ProgressBar
              value={active.progress.total === 0 ? 0 : active.progress.done / active.progress.total}
              label={
                active.kind === 'download'
                  ? 'downloading'
                  : active.kind === 'mounting'
                    ? 'mounting'
                    : 'syncing'
              }
              showValue
            />
          ) : null}

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
