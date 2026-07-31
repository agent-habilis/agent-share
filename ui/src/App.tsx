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
import type { Ctx } from 'visage-dom'

import { ColumnView } from './ColumnView.tsx'
import { saveZip, zipStream, type Progress } from './download.ts'
import { buildTree, filesUnder, humanBytes, type Manifest } from './tree.ts'

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

function Landing() {
  return (
    <Box border="line" padX={2} padY={1}>
      <Stack direction="column" gap={1}>
        <Text weight="bold">agent-share</Text>
        <Text color="fgMuted">Share a folder peer-to-peer. Open a link to browse one.</Text>
        <Text>agent-share serve ./some-folder</Text>
        <Text color="fgSubtle">
          The link it prints carries the whole capability in its fragment, so this site never
          sees it.
        </Text>
      </Stack>
    </Box>
  )
}

function Failed({ reason }: { reason: string }) {
  return (
    <Box border="line" padX={2} padY={1}>
      <Stack direction="column" gap={1}>
        <Text weight="bold" color="danger">
          Could not connect
        </Text>
        {/* No relayed data path exists by design, so a failed negotiation is
            the end of the road rather than a slower route. Say so. */}
        <Text color="fgMuted">
          A direct connection to this peer could not be established. Both ends may be behind
          restrictive NATs.
        </Text>
        <Text color="fgSubtle">{reason}</Text>
      </Stack>
    </Box>
  )
}

/**
 * One dialled session for a fixed ticket. Remounted (via `key`) when the
 * fragment changes so the previous watch/dial is disposed through ctx.aborted.
 */
const Session = component<{ ticket: string }>(function* (props, ctx: Ctx) {
  const state = signal<State>({ phase: 'connecting' })
  const path = signal<string[]>([])
  const progress = signal<Progress | null>(null)

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
      })
    } catch (error) {
      if (!ctx.aborted.aborted) state.value = { phase: 'failed', reason: String(error) }
    }
  })()

  const tree = computed(() => {
    const current = state.value
    return current.phase === 'ready' ? buildTree(current.manifest) : null
  })

  async function download(): Promise<void> {
    const current = state.peek()
    const built = tree.peek()
    if (current.phase !== 'ready' || !built) return
    const files = filesUnder(built.root)
    progress.value = {
      done: 0,
      total: files.reduce((sum, file) => sum + file.size, 0),
    }
    try {
      const stream = zipStream(current.client, files, (next) => {
        progress.value = next
      })
      await saveZip(stream, 'share.zip')
    } finally {
      progress.value = null
    }
  }

  yield () => {
    const current = state.value
    if (current.phase === 'connecting') {
      return (
        <div
          style={{
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'center',
            minHeight: 'calc(100vh - 2ch)',
            width: '100%',
          }}
        >
          <Stack direction="row" gap={1}>
            <Spinner />
            <Text>connecting over WebRTC…</Text>
          </Stack>
        </div>
      )
    }
    if (current.phase === 'failed') return <Failed reason={current.reason} />
    const built = tree.value
    if (current.phase !== 'ready' || !built) return null

    const files = filesUnder(built.root)
    const total = files.reduce((sum, file) => sum + file.size, 0)
    const prog = progress.value

    return (
      <Stack direction="column" gap={1}>
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
          <Button
            variant="primary"
            onclick={() => void download()}
            disabled={prog !== null}
          >
            Download
          </Button>
        </Stack>

        {prog ? (
          <ProgressBar
            value={prog.total === 0 ? 0 : prog.done / prog.total}
            label="downloading"
            showValue
          />
        ) : null}

        {built.skipped > 0 ? (
          <Text color="warning">
            {built.skipped} entries hidden — unsafe paths in the peer&apos;s manifest
          </Text>
        ) : null}

        <ColumnView
          root={built.root}
          path={path.value}
          onPathChange={(next) => {
            path.value = next
          }}
        />
      </Stack>
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
    if (!current) return <Landing />
    return <Session key={current} ticket={current} />
  }
})
