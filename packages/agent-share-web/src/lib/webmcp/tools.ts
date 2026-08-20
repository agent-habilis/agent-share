/**
 * The tools this page publishes to an agent.
 *
 * This is the agent's interface to a share, and only that. It does what a share
 * can do — open one, walk it, read a file, take a copy, publish a new one — and
 * says nothing about what is on screen; the UI is the person's interface and has
 * its own. Everything here is built on the same `lib/client`, `lib/tree` and
 * `lib/produce` the pages use, so an agent and a person drive one
 * implementation.
 *
 * Two rules hold for every tool in this file, both forced by how the browser
 * behaves rather than by taste (see `result.ts`):
 *
 * - **Check your own arguments.** `inputSchema` is documentation the agent
 *   reads, not a gate the browser enforces.
 * - **Never throw.** A throw reaches the agent as a generic "the invocation
 *   failed" with the message stripped, so failure is returned as data.
 *
 * Descriptions are model-facing text and are kept short and factual on purpose:
 * they are read on every tool listing, and they are the whole basis on which an
 * agent decides what to call.
 */

import { canUseOpfs, createShareDirectory, writeOpfsFile } from '../opfs/index.ts'
import { directorySource, startProducer, type ShareProducer } from '../produce.ts'
import { shareUrl, type TransportMode } from '../ticket/index.ts'
import { filesUnder, type FileNode } from '../tree.ts'
import { clampWindow, DEFAULT_READ_LEN, describeWindow, looksBinary, MAX_READ_LEN } from './bytes.ts'
import { collect, describeEntry, locate, requireFile, type Entry } from './entries.ts'
import { pathFilter } from './glob.ts'
import {
  fail,
  guard,
  ok,
  optionalBool,
  optionalInt,
  optionalString,
  requiredString,
  sharePathParts,
  ToolInputError,
  type ToolResult,
} from './result.ts'
import { openSession, requireSession, resolveTicket } from './session.ts'

/** Shared by every tool, so an agent can address a share it has not "opened". */
const TICKET_PROPERTY = {
  ticket: {
    type: 'string',
    description: 'Share ticket. Defaults to the share already open in this tab.',
  },
} as const

const PATH_PROPERTY = {
  path: {
    type: 'string',
    description: 'Path inside the share, like "src/lib.rs". Omit or "" for the root.',
  },
} as const

function object(properties: object, required: string[] = []): object {
  return { type: 'object', properties, required, additionalProperties: false }
}

// ---------------------------------------------------------------------------
// connect
// ---------------------------------------------------------------------------

const connect: ModelContextTool = {
  name: 'connect',
  description:
    'Open an agent-share peer-to-peer share and report what it holds and how it ' +
    'is connected. Call this before the other tools. Calling it again on a share ' +
    'that is already open refreshes the file list and the connection state ' +
    'rather than dialling a second time, so it doubles as a status check. Files ' +
    'are read over the network on demand; nothing is written to disk.',
  inputSchema: object({
    ticket: {
      type: 'string',
      description: 'Share ticket, or a share URL. Defaults to the one in this page URL.',
    },
    password: { type: 'string', description: 'Password, if the share is protected.' },
    transport: {
      type: 'string',
      enum: ['webrtc', 'relay', 'dynamic'],
      description: 'Force a data path. Omit to let the client choose.',
    },
  }),
  annotations: { readOnlyHint: true, untrustedContentHint: false },
  execute: (input) =>
    guard(async () => {
      const ticket = resolveTicket(optionalString(input, 'ticket'))
      const transport = optionalString(input, 'transport') as TransportMode | undefined
      const password = optionalString(input, 'password')
      const session = await openSession(ticket, { transport, password, refresh: true })

      // Counted off the tree, not the manifest. `buildTree` drops tombstones
      // and paths that would escape the root, so a manifest tally would report
      // files that `list` then refuses to show — two answers to one question,
      // in the same result that reports `skippedEntries`.
      return ok({
        ticket: session.ticket,
        url: shareUrl(session.ticket),
        files: session.root.files,
        directories: session.root.dirs,
        bytes: session.root.bytes,
        transport: session.client.transport,
        closed: session.client.closed,
        peersOnMesh: session.client.peers_gossip,
        peersDirect: session.client.peers_direct,
        maxDirect: session.client.max_direct,
        filesHeldLocally: session.client.held.length,
        // Non-zero means the peer named paths that would escape the share root.
        // Surfaced rather than swallowed: it says something about the peer.
        skippedEntries: session.skipped,
      })
    }),
}

// ---------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------

async function coverageOf(
  session: Awaited<ReturnType<typeof requireSession>>,
  index: number,
): Promise<number | undefined> {
  try {
    const map = (await session.client.coverage_map()) as Record<string, number> | null
    const value = map?.[String(index)]
    return typeof value === 'number' ? value : undefined
  } catch {
    // Coverage is a nicety on top of the answer, not the answer.
    return undefined
  }
}

const list: ModelContextTool = {
  name: 'list',
  description:
    'Describe a path inside an open share. A directory returns its entries with ' +
    'names, sizes and modification times, plus the total files and bytes beneath ' +
    'it. A file returns its size, modification time, and how much of it this ' +
    'browser already holds locally. Use it to find a path before reading one.',
  inputSchema: object({
    ...PATH_PROPERTY,
    depth: {
      type: 'integer',
      minimum: 1,
      maximum: 10,
      description: 'How many directory levels to include. Default 1. Ignored for a file.',
    },
    refresh: {
      type: 'boolean',
      description: 'Refetch the file list. Use if the share may have changed.',
    },
    ...TICKET_PROPERTY,
  }),
  annotations: { readOnlyHint: true, untrustedContentHint: true },
  execute: (input) =>
    guard(async () => {
      const refresh = optionalBool(input, 'refresh', false)
      const depth = optionalInt(input, 'depth', { min: 1, max: 10, fallback: 1 })
      const session = await requireSession(optionalString(input, 'ticket'), refresh)
      const parts = sharePathParts(optionalString(input, 'path'))
      const node = locate(session.root, parts)

      if (node.kind === 'file') {
        const held = session.client.held.includes(node.index)
        return ok({
          ...describeEntry(node),
          index: node.index,
          held,
          coverage: held ? 1 : ((await coverageOf(session, node.index)) ?? 0),
        })
      }

      const entries: Entry[] = []
      collect(node, depth, entries)
      return ok({
        kind: 'dir' as const,
        path: node.path,
        entries,
        count: entries.length,
        files: node.files,
        bytes: node.bytes,
      })
    }),
}

// ---------------------------------------------------------------------------
// read
// ---------------------------------------------------------------------------

async function readWindow(
  session: Awaited<ReturnType<typeof requireSession>>,
  file: FileNode,
  offset: number,
  len: number,
): Promise<Uint8Array> {
  const window = clampWindow(file.size, offset, len)
  if (window.len === 0) return new Uint8Array(0)
  return session.client.read(file.index, BigInt(window.offset), window.len)
}

const read: ModelContextTool = {
  name: 'read',
  description:
    'Read part of a file from an open share. Returns UTF-8 text, or base64 when ' +
    'the bytes are binary. Reads are windowed: follow nextOffset in the result ' +
    'to continue, and stop when eof is true.',
  inputSchema: object(
    {
      path: {
        type: 'string',
        description: 'Path of the file to read, like "src/lib.rs".',
      },
      offset: {
        type: 'integer',
        minimum: 0,
        description: 'Byte offset to start at. Default 0.',
      },
      length: {
        type: 'integer',
        minimum: 1,
        maximum: MAX_READ_LEN,
        description: `Bytes to read, up to ${MAX_READ_LEN}. Default ${DEFAULT_READ_LEN}.`,
      },
      ...TICKET_PROPERTY,
    },
    ['path'],
  ),
  annotations: { readOnlyHint: true, untrustedContentHint: true },
  execute: (input) =>
    guard(async () => {
      const rawPath = requiredString(input, 'path')
      const offset = optionalInt(input, 'offset', {
        min: 0,
        max: Number.MAX_SAFE_INTEGER,
        fallback: 0,
      })
      const length = optionalInt(input, 'length', {
        min: 1,
        max: MAX_READ_LEN,
        fallback: DEFAULT_READ_LEN,
      })
      const session = await requireSession(optionalString(input, 'ticket'))
      const parts = sharePathParts(rawPath)
      if (parts.length === 0) return fail('bad_argument', '"path" must name a file')
      const file = requireFile(session.root, parts)

      const bytes = await readWindow(session, file, offset, length)
      return ok({ path: file.path, ...describeWindow(bytes, Math.min(offset, file.size), file.size) })
    }),
}

// ---------------------------------------------------------------------------
// search
// ---------------------------------------------------------------------------

/**
 * Bounds on a search, so one call cannot pull a whole share over the wire.
 *
 * Every one of these is reported back when it bites. A search that silently
 * stopped early would read as "there are no more matches", which is a different
 * answer to "I stopped looking".
 */
const SEARCH_MAX_FILES = 200
const SEARCH_MAX_FILE_BYTES = 1048576
const SEARCH_LINE_CHARS = 300

interface Match {
  path: string
  line: number
  text: string
}

const search: ModelContextTool = {
  name: 'search',
  description:
    'Search the text files of an open share for a string and return matching ' +
    'lines with their paths and line numbers. Narrow it with a glob such as ' +
    '"*.rs" or "src/**". Binary and very large files are skipped.',
  inputSchema: object(
    {
      query: { type: 'string', description: 'Text to look for.' },
      glob: {
        type: 'string',
        description: 'Limit to matching paths, e.g. "*.ts" or "src/**". Optional.',
      },
      ignoreCase: { type: 'boolean', description: 'Case-insensitive match. Default false.' },
      maxResults: {
        type: 'integer',
        minimum: 1,
        maximum: 500,
        description: 'Most matches to return. Default 50.',
      },
      ...TICKET_PROPERTY,
    },
    ['query'],
  ),
  annotations: { readOnlyHint: true, untrustedContentHint: true },
  execute: (input) =>
    guard(async () => {
      const query = requiredString(input, 'query')
      const glob = optionalString(input, 'glob')
      const ignoreCase = optionalBool(input, 'ignoreCase', false)
      const maxResults = optionalInt(input, 'maxResults', { min: 1, max: 500, fallback: 50 })
      const session = await requireSession(optionalString(input, 'ticket'))

      const matches = pathFilter(glob)
      const candidates = filesUnder(session.root).filter((file) => matches(file.path))
      const needle = ignoreCase ? query.toLowerCase() : query

      const results: Match[] = []
      const skipped: { path: string; reason: string }[] = []
      let scanned = 0
      let truncated = false

      for (const file of candidates) {
        if (results.length >= maxResults) {
          truncated = true
          break
        }
        if (scanned >= SEARCH_MAX_FILES) {
          truncated = true
          break
        }
        if (file.size > SEARCH_MAX_FILE_BYTES) {
          skipped.push({ path: file.path, reason: 'too large' })
          continue
        }
        if (file.size === 0) continue

        let bytes: Uint8Array
        try {
          bytes = await readWindow(session, file, 0, Math.min(file.size, MAX_READ_LEN))
        } catch (error) {
          skipped.push({ path: file.path, reason: `unreadable: ${String(error)}` })
          continue
        }
        scanned += 1
        if (looksBinary(bytes)) {
          skipped.push({ path: file.path, reason: 'binary' })
          continue
        }

        const text = new TextDecoder().decode(bytes)
        const lines = text.split('\n')
        for (let index = 0; index < lines.length; index += 1) {
          const line = lines[index] as string
          const haystack = ignoreCase ? line.toLowerCase() : line
          if (!haystack.includes(needle)) continue
          results.push({
            path: file.path,
            line: index + 1,
            text: line.length > SEARCH_LINE_CHARS ? `${line.slice(0, SEARCH_LINE_CHARS)}…` : line,
          })
          if (results.length >= maxResults) {
            truncated = true
            break
          }
        }
      }

      return ok({
        query,
        matches: results,
        filesScanned: scanned,
        filesMatchingGlob: candidates.length,
        // Only ever present when something really was left out.
        ...(skipped.length > 0 ? { skipped } : {}),
        truncated,
      })
    }),
}

// ---------------------------------------------------------------------------
// sync
// ---------------------------------------------------------------------------

const sync: ModelContextTool = {
  name: 'sync',
  description:
    'Download files from an open share into this browser so later reads are ' +
    'local and this tab helps serve them to other peers. Stores in browser ' +
    'storage, not on the filesystem.',
  inputSchema: object({
    only: {
      type: 'array',
      items: { type: 'string' },
      description: 'Paths to take — a file, or a folder and all under it. Omit for all.',
    },
    ...TICKET_PROPERTY,
  }),
  annotations: { readOnlyHint: false, untrustedContentHint: false },
  execute: (input) =>
    guard(async () => {
      const raw = input['only']
      let only: string[] | undefined
      if (raw !== undefined && raw !== null) {
        if (!Array.isArray(raw)) return fail('bad_argument', '"only" must be an array of paths')
        only = raw.map((entry) => sharePathParts(String(entry)).join('/')).filter(Boolean)
      }

      const session = await requireSession(optionalString(input, 'ticket'))
      const stats = await session.client.sync(only)
      // Adopt what landed and tell the mesh, so this tab becomes a seeder.
      // Done once here rather than per chunk; see the note on `lib/client`.
      await session.client.republish_holdings()
      return ok({
        files: stats.files,
        bytes: stats.bytes,
        verified: stats.verified,
        unverified: stats.unverified,
        skipped: stats.skipped,
        held: stats.held,
      })
    }),
}

// ---------------------------------------------------------------------------
// publish
// ---------------------------------------------------------------------------

/**
 * Producers this page started, kept so they are not collected mid-serve.
 *
 * A published share lives exactly as long as the tab. There is no stop tool by
 * design: an agent that could stop a share could stop one a person is using,
 * and closing the tab is already the honest way to end it.
 */
const published: ShareProducer[] = []

function decodeContent(
  content: string,
  encoding: string | undefined,
): string | Uint8Array<ArrayBuffer> {
  if (encoding === undefined || encoding === 'utf8') return content
  if (encoding !== 'base64') {
    throw new ToolInputError('bad_argument', '"encoding" must be "utf8" or "base64"')
  }
  const binary = atob(content)
  const bytes = new Uint8Array(binary.length)
  for (let index = 0; index < binary.length; index += 1) {
    bytes[index] = binary.charCodeAt(index)
  }
  return bytes
}

const publish: ModelContextTool = {
  name: 'publish',
  description:
    'Publish files as a new peer-to-peer share and return a ticket and URL that ' +
    'others can open. Files are held in this browser, so the share lasts only ' +
    'while this tab stays open. Cannot share an existing folder on disk.',
  inputSchema: object(
    {
      files: {
        type: 'array',
        minItems: 1,
        description: 'The files to publish.',
        items: object(
          {
            path: { type: 'string', description: 'Path within the share, e.g. "docs/readme.md".' },
            content: { type: 'string', description: 'File contents.' },
            encoding: {
              type: 'string',
              enum: ['utf8', 'base64'],
              description: 'How content is encoded. Default utf8.',
            },
          },
          ['path', 'content'],
        ),
      },
      password: { type: 'string', description: 'Require this password to open the share.' },
    },
    ['files'],
  ),
  annotations: { readOnlyHint: false, untrustedContentHint: false },
  execute: (input) =>
    guard(async () => {
      if (!canUseOpfs()) {
        return fail('unsupported', 'This browser cannot store files for a share.')
      }
      const raw = input['files']
      if (!Array.isArray(raw) || raw.length === 0) {
        return fail('bad_argument', '"files" must be a non-empty array')
      }

      const { handle } = await createShareDirectory('agent-share')
      for (const entry of raw) {
        if (typeof entry !== 'object' || entry === null) {
          return fail('bad_argument', 'each item in "files" must be an object')
        }
        const record = entry as Record<string, unknown>
        const path = sharePathParts(requiredString(record, 'path')).join('/')
        if (!path) return fail('bad_argument', 'each file needs a "path"')
        const content = record['content']
        if (typeof content !== 'string') {
          return fail('bad_argument', `"content" for "${path}" must be a string`)
        }
        await writeOpfsFile(handle, path, decodeContent(content, optionalString(record, 'encoding')))
      }

      const producer = await startProducer(directorySource(handle), optionalString(input, 'password'))
      published.push(producer)
      return ok({
        ticket: producer.ticket,
        url: shareUrl(producer.ticket),
        files: producer.files,
        bytes: producer.bytes,
        passwordProtected: producer.passwordProtected,
        note: 'This share stays available only while this browser tab is open.',
      })
    }),
}

export const TOOLS: readonly ModelContextTool[] = [
  connect,
  list,
  read,
  search,
  sync,
  publish,
]

export type { ToolResult }
