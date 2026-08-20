/**
 * Which share a tool call is talking about, and the client that serves it.
 *
 * A tool call is a single request with no memory, but a share connection is a
 * live WebRTC session that takes a round of ICE to build. So the ticket is
 * resolved per call and the connection is not: `shareConnect` establishes one,
 * and every later call finds it again.
 *
 * The connection itself is **not** cached here. `connect()` in `lib/client`
 * already keeps one client per ticket and transport, which is what makes an
 * agent and a human co-exist on one page: an agent calling `shareConnect` for
 * the ticket already open in the tab gets handed the session the UI is using,
 * rather than opening a second one. A second cache in front of it would undo
 * that.
 *
 * What *is* kept here is the manifest. Fetching it is a round trip, and the
 * obvious alternative — subscribing with `watch()` — is wrong: the connection
 * may already carry the UI's subscription, and a second one cannot be cancelled
 * (see the note on `release` in `lib/client`). So the tree is cached and
 * refreshed when a caller asks, rather than pushed.
 */

import { type Client, connect, isUnauthorized } from '../client/index.ts'
import { parseRoute, type TransportMode } from '../ticket/index.ts'
import { buildTree, type DirNode, type Manifest } from '../tree.ts'
import { ToolInputError } from './result.ts'

export interface Session {
  ticket: string
  client: Client
  manifest: Manifest
  root: DirNode
  /** Manifest entries dropped for naming a path outside the share. */
  skipped: number
}

interface Entry {
  ticket: string
  transport: TransportMode | undefined
  client: Client
  tree?: { manifest: Manifest; root: DirNode; skipped: number }
}

const sessions = new Map<string, Entry>()

/** The ticket the last `shareConnect` used, so later calls can omit it. */
let lastTicket: string | undefined

/**
 * Work out which share is meant.
 *
 * In order: what the caller said, what was last connected, and what the page's
 * own URL names. The last one matters more than it looks — an agent that has
 * navigated to `/files/<ticket>` has already said which share it means, and
 * making it repeat the ticket in every call would be asking for a value the
 * page is holding.
 */
export function resolveTicket(explicit?: string): string {
  const fromUrl = safeRouteTicket()
  const ticket = explicit ?? lastTicket ?? fromUrl
  if (!ticket) {
    throw new ToolInputError(
      'no_session',
      'No share is open. Pass "ticket", or open /files/<ticket> first.',
    )
  }
  return ticket
}

function safeRouteTicket(): string | undefined {
  try {
    return parseRoute()?.ticket
  } catch {
    return undefined
  }
}

/**
 * Connect if needed and return the session, with its tree loaded.
 *
 * `refresh` refetches the manifest. A share is live — a producer can add or
 * remove files while an agent is reading it — so a long sequence of calls
 * against a cached tree can describe a share that has moved on.
 */
export async function openSession(
  ticket: string,
  options: { transport?: TransportMode; password?: string; refresh?: boolean } = {},
): Promise<Session> {
  const key = `${options.transport ?? 'dynamic'} ${ticket}`
  let entry = sessions.get(key)

  if (entry?.client.closed) {
    sessions.delete(key)
    entry = undefined
  }

  if (!entry) {
    let client: Client
    try {
      client = await connect(ticket, options.transport, undefined, options.password)
    } catch (error) {
      if (isUnauthorized(error)) {
        throw new ToolInputError(
          'unauthorized',
          'This share needs a password, or the one given was wrong. Pass "password".',
        )
      }
      throw error
    }
    entry = { ticket, transport: options.transport, client }
    sessions.set(key, entry)
  }

  if (!entry.tree || options.refresh) {
    const manifest = await entry.client.manifest()
    const { root, skipped } = buildTree(manifest)
    entry.tree = { manifest, root, skipped }
  }

  lastTicket = ticket
  return {
    ticket,
    client: entry.client,
    manifest: entry.tree.manifest,
    root: entry.tree.root,
    skipped: entry.tree.skipped,
  }
}

/**
 * The session for a call that is not `shareConnect`.
 *
 * Deliberately does not take a password: a tool that would silently dial a new
 * connection makes `shareConnect` look optional, and then a wrong password
 * surfaces from whichever tool happened to run first.
 */
export async function requireSession(
  explicit: string | undefined,
  refresh = false,
): Promise<Session> {
  return openSession(resolveTicket(explicit), { refresh })
}

/**
 * The cached tree for `ticket`, without dialling anything.
 *
 * For callers that want to check a path against the share but must not pay a
 * connection to do it — the UI tools, which are moving a view the user already
 * has open and should not open a second session to validate a click.
 */
export function peekTree(ticket: string): DirNode | null {
  for (const entry of sessions.values()) {
    if (entry.ticket === ticket && entry.tree) return entry.tree.root
  }
  return null
}

/** Forget every session. Test seam; the clients themselves are owned by `lib/client`. */
export function resetSessions(): void {
  sessions.clear()
  lastTicket = undefined
}
