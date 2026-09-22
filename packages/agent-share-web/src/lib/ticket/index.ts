/**
 * Share tickets and path-based share URLs.
 *
 * Routes:
 * - `/` — home
 * - `/files/<ticket>` — file browser
 * - `/info/<ticket>` — session info
 * - `/preview/<ticket>/<path…>` — one file, rendered in place
 *
 * The ticket is a bearer capability in one path segment. It is bare ASCII
 * Base58, so it survives a URL path verbatim — no percent-encoding.
 *
 * Preview carries the selected file in the path rather than a query param, so
 * the URL reads as what it names and a preview survives a reload. Every segment
 * is percent-encoded on the way out and decoded on the way back, because a file
 * name may hold anything a filesystem allows — `?`, `#`, a literal `/` cannot
 * appear, but a `%` can.
 *
 * `?transport=webrtc|dynamic` pins the mount data path (the wasm
 * `TransportMode`). `?dev=true` reveals the Info pane's dev tools. Both are
 * local debugging preferences rather than part of the capability, so they ride
 * the query string — and `shareUrl`, the link a producer hands out,
 * deliberately leaves them off.
 */

export type ShareView = 'files' | 'info' | 'preview'

/** Mount data path, spelled as the wasm `TransportMode` spells it. */
export type TransportMode = 'webrtc' | 'dynamic'

export interface ShareRoute {
  view: ShareView
  ticket: string
  /**
   * The file the `preview` view names, one segment per tree level. Empty on
   * every other view, and on a `/preview/<ticket>` that names no file.
   */
  path: string[]
  /** Requested data path. Absent ⇒ the wasm default, `dynamic`. */
  transport?: TransportMode
  /** Show the Info pane's dev tools. Absent ⇒ hidden. */
  dev?: boolean
}

const VIEW_RE = /^(files|info|preview)$/

const TRANSPORT_MODES: readonly TransportMode[] = ['webrtc', 'dynamic']

/**
 * Read `?transport=` out of a query string.
 *
 * Canonical names only. `TransportMode::parse` on the wasm side also takes
 * aliases (`webrtc_only`, `webrtc-only`, …) for direct API callers, but four
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

/**
 * Path for a share view, with the ticket percent-encoded as one segment.
 *
 * `file` is the preview view's target, one segment per tree level. It trails
 * the ticket rather than riding the query string, and `transport`/`dev` keep
 * their positions ahead of it so the existing callers read unchanged.
 */
export function sharePath(
  ticket: string,
  view: ShareView = 'files',
  transport?: TransportMode,
  dev = false,
  file: readonly string[] = [],
): string {
  const segments = [view, ticket, ...file].map(encodeURIComponent)
  const path = `/${segments.join('/')}`
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
 * Parse `/files/<ticket>`, `/info/<ticket>` or `/preview/<ticket>/<path…>`.
 * Returns `null` for home or anything that is not a share route.
 *
 * Only `preview` accepts trailing segments; a stray one on the other two views
 * is still a mistake and still reads as home. The path it yields is a lookup
 * key into the manifest-derived tree and never touches a filesystem, so a
 * segment naming nothing simply finds nothing.
 */
export function parseRoute(
  pathname: string = window.location.pathname,
  search: string = window.location.search,
): ShareRoute | null {
  const parts = pathname.replace(/\/+$/, '').split('/').filter(Boolean)
  const [viewRaw, ticketRaw, ...rest] = parts
  if (!viewRaw || !ticketRaw || !VIEW_RE.test(viewRaw)) return null
  const view = viewRaw as ShareView
  if (rest.length > 0 && view !== 'preview') return null
  let ticket: string
  let path: string[]
  try {
    ticket = decodeURIComponent(ticketRaw).trim()
    path = rest.map(decodeURIComponent)
  } catch {
    return null
  }
  if (!ticket) return null
  const route: ShareRoute = { view, ticket, path }
  const transport = parseTransport(search)
  if (transport) route.transport = transport
  if (parseDev(search)) route.dev = true
  return route
}

/**
 * The file a `/preview/<ticket>/<path…>` URL names, one segment per tree level.
 *
 * Read off the raw pathname rather than the router's wildcard param, because
 * the param arrives already percent-decoded and splitting *after* decoding is
 * the wrong order — a segment holding a `%2F` would split into two.
 */
export function previewSegments(pathname: string = window.location.pathname): string[] {
  const parts = pathname.replace(/\/+$/, '').split('/').filter(Boolean)
  try {
    return parts.slice(2).map(decodeURIComponent)
  } catch {
    return []
  }
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
