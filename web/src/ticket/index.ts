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
 *
 * `?transport=webrtc|relay|dynamic` pins the mount data path (the wasm
 * `TransportMode`). `?dev=true` reveals the Info pane's dev tools. Both are
 * local debugging preferences rather than part of the capability, so they ride
 * the query string — and `shareUrl`, the link a producer hands out,
 * deliberately leaves them off.
 */

export type ShareView = 'files' | 'info'

/** Mount data path, spelled as the wasm `TransportMode` spells it. */
export type TransportMode = 'webrtc' | 'relay' | 'dynamic'

export interface ShareRoute {
  view: ShareView
  ticket: string
  /** Requested data path. Absent ⇒ the wasm default, `dynamic`. */
  transport?: TransportMode
  /** Show the Info pane's dev tools. Absent ⇒ hidden. */
  dev?: boolean
}

const VIEW_RE = /^(files|info)$/

const TRANSPORT_MODES: readonly TransportMode[] = ['webrtc', 'relay', 'dynamic']

/**
 * Read `?transport=` out of a query string.
 *
 * Canonical names only. `TransportMode::parse` on the wasm side also takes
 * aliases (`webrtc_only`, `iroh-relay`, …) for direct API callers, but four
 * spellings per mode is not a URL surface anyone can eyeball. An unrecognised
 * value reads as absent, so a typo degrades to the default rather than failing
 * the page.
 */
export function parseTransport(
  search: string = window.location.search,
): TransportMode | undefined {
  const raw = new URLSearchParams(search).get('transport')?.trim().toLowerCase()
  return TRANSPORT_MODES.find((mode) => mode === raw)
}

/**
 * Read `?dev=` out of a query string.
 *
 * As strict as [`parseTransport`], and for the same reason: an unrecognised
 * value reads as off, so a typo hides the dev tools rather than failing the
 * page. `dev` alone (no value) does not count — an explicit `true`/`1` keeps
 * the flag hard to set by accident.
 */
export function parseDev(search: string = window.location.search): boolean {
  const raw = new URLSearchParams(search).get('dev')?.trim().toLowerCase()
  return raw === 'true' || raw === '1'
}

/** Path for a share view, with the ticket percent-encoded as one segment. */
export function sharePath(
  ticket: string,
  view: ShareView = 'files',
  transport?: TransportMode,
  dev = false,
): string {
  const path = `/${view}/${encodeURIComponent(ticket)}`
  const query = new URLSearchParams()
  if (transport) query.set('transport', transport)
  if (dev) query.set('dev', 'true')
  const search = query.toString()
  return search ? `${path}?${search}` : path
}

/**
 * Absolute share URL peers open. Defaults to the files view.
 *
 * No `transport` and no `dev`, on purpose: this is the link that leaves the
 * machine, and neither pinning one tab's debugging transport nor opening a dev
 * pane on every peer who receives it is the intent.
 */
export function shareUrl(ticket: string, view: ShareView = 'files'): string {
  return `${window.location.origin}${sharePath(ticket, view)}`
}

/**
 * Parse `/files/<ticket>` or `/info/<ticket>`. Returns `null` for home or
 * anything that is not a share route.
 */
export function parseRoute(
  pathname: string = window.location.pathname,
  search: string = window.location.search,
): ShareRoute | null {
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
  const route: ShareRoute = { view: viewRaw as ShareView, ticket }
  const transport = parseTransport(search)
  if (transport) route.transport = transport
  if (parseDev(search)) route.dev = true
  return route
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

/**
 * Push (or replace) a share route and notify subscribers.
 *
 * Carries the current `?transport=` and `?dev=` forward. Without that,
 * switching `/files` ↔ `/info` would drop the pin and silently redial the share
 * in a different mode — so the Info pane you opened to inspect a WebRTC session
 * would be reporting on a fresh dynamic one. `dev` rides along for the plainer
 * reason that the dev tools live *in* that pane, so dropping the flag on the
 * way to it would make them unreachable.
 */
export function navigateToShare(
  ticket: string,
  view: ShareView = 'files',
  options?: { replace?: boolean },
): void {
  const path = sharePath(ticket, view, parseTransport(), parseDev())
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
      const fromPath = parseRoute(url.pathname, url.search)
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
