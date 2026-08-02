/**
 * Parse a paste of either a bare `🐝…` ticket or a share URL whose fragment
 * carries one. The fragment is the only place a ticket belongs — never the path.
 */

/** Pull a ticket out of a prompt paste. `null` when there is nothing usable. */
export function parseShareInput(raw: string): string | null {
  const trimmed = raw.trim()
  if (!trimmed) return null

  // Full URL (or anything with a `#fragment`).
  if (trimmed.includes('#')) {
    try {
      const url = trimmed.includes('://')
        ? new URL(trimmed)
        : new URL(trimmed, window.location.origin)
      const fragment = decodeURIComponent(url.hash.replace(/^#/, '')).trim()
      return fragment || null
    } catch {
      const hash = trimmed.indexOf('#')
      const fragment = decodeURIComponent(trimmed.slice(hash + 1)).trim()
      return fragment || null
    }
  }

  return trimmed
}

/** The share URL peers open — ticket rides in the fragment only. */
export function shareUrl(ticket: string): string {
  return `${window.location.origin}${window.location.pathname}#${encodeURIComponent(ticket)}`
}
