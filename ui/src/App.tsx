/**
 * The share browser.
 *
 * The ticket lives in the URL *fragment*, never the path: it is a bearer
 * capability granting full read access, and a path would send it to the server
 * on every request — into logs, proxies and referrers. A fragment never leaves
 * the browser, which is what lets this be a purely static site.
 */

import { Badge, Box, Button, ProgressBar, Spinner, Stack, Text } from 'moonspace-ui'
import { useCallback, useEffect, useMemo, useState } from 'react'

import { ColumnView } from './ColumnView.tsx'
import { saveZip, zipStream, type Progress } from './download.ts'
import { buildTree, filesUnder, humanBytes, type Manifest } from './tree.ts'

interface Client {
  readonly transport: string
  manifest(): Promise<Manifest>
  read(index: number, offset: bigint, len: number): Promise<Uint8Array>
}

type State =
  | { phase: 'idle' }
  | { phase: 'connecting' }
  | { phase: 'ready'; client: Client; manifest: Manifest }
  | { phase: 'failed'; reason: string }

export function App() {
  const [state, setState] = useState<State>({ phase: 'idle' })
  const [path, setPath] = useState<string[]>([])
  const [progress, setProgress] = useState<Progress | null>(null)

  const ticket = useTicket()

  useEffect(() => {
    if (!ticket) return
    let cancelled = false
    setState({ phase: 'connecting' })
    void (async () => {
      try {
        const wasm = await import('../../crates/agent-share-wasm-client/dist/web/agent_share_wasm_client.js')
        await wasm.default()
        const client = (await wasm.ShareClient.connect(ticket)) as unknown as Client
        const manifest = await client.manifest()
        if (!cancelled) setState({ phase: 'ready', client, manifest })
      } catch (error) {
        if (!cancelled) setState({ phase: 'failed', reason: String(error) })
      }
    })()
    return () => {
      cancelled = true
    }
  }, [ticket])

  const tree = useMemo(
    () => (state.phase === 'ready' ? buildTree(state.manifest) : null),
    [state],
  )

  const download = useCallback(async () => {
    if (state.phase !== 'ready' || !tree) return
    const files = filesUnder(tree.root)
    setProgress({ done: 0, total: files.reduce((sum, file) => sum + file.size, 0) })
    try {
      const stream = zipStream(state.client, files, setProgress)
      await saveZip(stream, 'share.zip')
    } finally {
      setProgress(null)
    }
  }, [state, tree])

  if (!ticket) return <Landing />
  if (state.phase === 'connecting') {
    return (
      <Stack direction="row" gap={1}>
        <Spinner />
        <Text>connecting over WebRTC…</Text>
      </Stack>
    )
  }
  if (state.phase === 'failed') return <Failed reason={state.reason} />
  if (state.phase !== 'ready' || !tree) return null

  const files = tree.root ? filesUnder(tree.root) : []
  const total = files.reduce((sum, file) => sum + file.size, 0)

  return (
    <Stack direction="column" gap={1}>
      <Stack direction="row" gap={2} justify="between">
        <Stack direction="row" gap={1}>
          <Text weight="bold">agent-share</Text>
          <Badge tone="success" variant="outline">
            {state.client.transport}
          </Badge>
          <Text color="fgMuted">
            {files.length} files · {humanBytes(total)}
          </Text>
        </Stack>
        <Button variant="primary" onClick={() => void download()} disabled={progress !== null}>
          Download
        </Button>
      </Stack>

      {progress && (
        <ProgressBar
          value={progress.total === 0 ? 0 : progress.done / progress.total}
          label="downloading"
          showValue
        />
      )}

      {tree.skipped > 0 && (
        <Text color="warning">
          {tree.skipped} entries hidden — unsafe paths in the peer&apos;s manifest
        </Text>
      )}

      <ColumnView root={tree.root} path={path} onPathChange={setPath} />
    </Stack>
  )
}

/** The ticket from `location.hash`, kept in sync with back/forward. */
function useTicket(): string | null {
  const read = () => decodeURIComponent(window.location.hash.replace(/^#/, '')).trim() || null
  const [ticket, setTicket] = useState<string | null>(read)
  useEffect(() => {
    const onHashChange = () => setTicket(read())
    window.addEventListener('hashchange', onHashChange)
    return () => window.removeEventListener('hashchange', onHashChange)
  }, [])
  return ticket
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
