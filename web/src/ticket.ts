/**
 * Share tickets and path-based share URLs.
 *
 * Routes:
 * - `/` — home
 * - `/files/<ticket>` — file browser
 * - `/info/<ticket>` — session info
 *
 * The ticket is a bearer capability in one path segment. It is bare ASCII
 * Base58, so it survives a URL path verbatim — no percent-encoding.
 */

export type ShareView = 'files' | 'info'

export interface ShareRoute {
  view: ShareView
  ticket: string
}

const VIEW_RE = /^(files|info)$/

/** Path for a share view, with the ticket percent-encoded as one segment. */
export function sharePath(ticket: string, view: ShareView = 'files'): string {
  return `/${view}/${encodeURIComponent(ticket)}`
}

/** Absolute share URL peers open. Defaults to the files view. */
export function shareUrl(ticket: string, view: ShareView = 'files'): string {
  return `${window.location.origin}${sharePath(ticket, view)}`
}

/**
 * Parse `/files/<ticket>` or `/info/<ticket>`. Returns `null` for home or
 * anything that is not a share route.
 */
export function parseRoute(pathname: string = window.location.pathname): ShareRoute | null {
  const parts = pathname.replace(/\/+$/, '').split('/').filter(Boolean)
  if (parts.length !== 2) return null
  const [viewRaw, ticketRaw] = parts
  if (!viewRaw || !ticketRaw || !VIEW_RE.test(viewRaw)) return null
  let ticket: string
  try {
    ticket = decodeURIComponent(ticketRaw).trim()
  } catch {
    return null
  }
  if (!ticket) return null
  return { view: viewRaw as ShareView, ticket }
}

type RouteListener = () => void
const listeners = new Set<RouteListener>()

/** Subscribe to in-app navigations and `popstate`. Returns an unsubscribe. */
export function onRouteChange(listener: RouteListener): () => void {
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
  }
}

function notifyRouteChange(): void {
  for (const listener of listeners) listener()
}

/** Push (or replace) a share route and notify subscribers. */
export function navigateToShare(
  ticket: string,
  view: ShareView = 'files',
  options?: { replace?: boolean },
): void {
  const path = sharePath(ticket, view)
  if (options?.replace) {
    window.history.replaceState(null, '', path)
  } else {
    window.history.pushState(null, '', path)
  }
  notifyRouteChange()
}

/** Pull a ticket out of a prompt paste. `null` when there is nothing usable. */
export function parseShareInput(raw: string): string | null {
  const trimmed = raw.trim()
  if (!trimmed) return null

  // Absolute or origin-relative URL.
  if (trimmed.includes('://') || trimmed.startsWith('/')) {
    try {
      const url = trimmed.includes('://')
        ? new URL(trimmed)
        : new URL(trimmed, window.location.origin)
      const fromPath = parseRoute(url.pathname)
      if (fromPath) return fromPath.ticket
    } catch {
      // Fall through.
    }
    return null
  }

  // Fragment-only pastes are not share URLs.
  if (trimmed.startsWith('#')) return null

  return trimmed
}

// Bridge `popstate` (back/forward) into the same listener set as pushState.
if (typeof window !== 'undefined') {
  window.addEventListener('popstate', () => {
    notifyRouteChange()
  })
}
