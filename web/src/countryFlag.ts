/**
 * Country-flag emoji helpers for peer IP display.
 *
 * Flag emojis are a pair of Unicode regional-indicator symbols derived from an
 * ISO 3166-1 alpha-2 code (e.g. `US` → 🇺🇸). IP → country is a separate,
 * injectable lookup so tests never hit the network.
 */

const REGIONAL_A = 0x1f1e6 // Regional Indicator Symbol Letter A
const ASCII_A = 0x41

/** RFC1918 / special ranges we never ask a GeoIP service about. */
const PRIVATE_V4 = [
  /^10\./,
  /^127\./,
  /^0\./,
  /^169\.254\./,
  /^192\.168\./,
  /^172\.(1[6-9]|2\d|3[0-1])\./,
  /^100\.(6[4-9]|[7-9]\d|1[0-1]\d|12[0-7])\./, // CGNAT 100.64/10
  /^192\.0\.0\./,
  /^192\.0\.2\./, // TEST-NET-1
  /^198\.51\.100\./, // TEST-NET-2
  /^203\.0\.113\./, // TEST-NET-3
  /^233\.252\.0\./,
]

/**
 * Normalize a country code to ISO 3166-1 alpha-2 uppercase, or `null` if it is
 * not a plausible two-letter code. Accepts surrounding whitespace and common
 * aliases (`UK` → `GB`).
 */
export function normalizeCountryCode(code: string | null | undefined): string | null {
  if (code == null) return null
  let trimmed = code.trim().toUpperCase()
  if (trimmed === 'UK') trimmed = 'GB'
  if (trimmed.length !== 2) return null
  if (trimmed[0]! < 'A' || trimmed[0]! > 'Z') return null
  if (trimmed[1]! < 'A' || trimmed[1]! > 'Z') return null
  // AA, QM–QZ, XA–XZ, ZZ are user-assigned / not real countries — still valid
  // as regional indicators, so we allow them (callers can filter).
  return trimmed
}

/**
 * Convert an ISO 3166-1 alpha-2 country code to a flag emoji.
 *
 * Returns `null` when the input is not a two-letter A–Z code (after
 * {@link normalizeCountryCode}).
 *
 * @example
 * countryCodeToFlag('us') // '🇺🇸'
 * countryCodeToFlag('UK') // '🇬🇧'
 * countryCodeToFlag('xx') // '🇽🇽' (valid regional pair; not a real flag)
 * countryCodeToFlag('USA') // null
 */
export function countryCodeToFlag(code: string | null | undefined): string | null {
  const normalized = normalizeCountryCode(code)
  if (normalized === null) return null
  const first = REGIONAL_A + (normalized.charCodeAt(0) - ASCII_A)
  const second = REGIONAL_A + (normalized.charCodeAt(1) - ASCII_A)
  return String.fromCodePoint(first, second)
}

/**
 * Whether `ip` is a candidate for a public GeoIP lookup.
 *
 * Returns false for empty values, mDNS `.local` names, IPv6 ULA/link-local/
 * loopback, and common private/test IPv4 ranges.
 */
export function isGeoLookupCandidate(ip: string | null | undefined): boolean {
  if (ip == null) return false
  const value = ip.trim()
  if (value.length === 0) return false
  if (value.endsWith('.local')) return false
  if (value.includes('%')) return false // zone id on link-local

  // IPv6
  if (value.includes(':')) {
    const lower = value.toLowerCase()
    if (lower === '::1') return false
    if (lower.startsWith('fe80:')) return false // link-local
    if (lower.startsWith('fc') || lower.startsWith('fd')) return false // ULA
    if (lower.startsWith('::ffff:')) {
      // IPv4-mapped — check the embedded v4
      const v4 = lower.slice('::ffff:'.length)
      return isGeoLookupCandidate(v4)
    }
    return true
  }

  // IPv4
  if (!/^\d{1,3}(\.\d{1,3}){3}$/.test(value)) return false
  const octets = value.split('.').map((part) => Number(part))
  if (octets.some((n) => !Number.isInteger(n) || n < 0 || n > 255)) return false
  return !PRIVATE_V4.some((pattern) => pattern.test(value))
}

export interface FormatIpWithFlagOptions {
  /** ISO country code; when valid, its flag prefixes the label. */
  countryCode?: string | null
  /** ICE candidate kind (`host` / `srflx` / `relay` / `mdns`), shown in parens. */
  kind?: string | null
}

/**
 * Format an IP for the peers table, optionally prefixing a country flag.
 *
 * @example
 * formatIpWithFlag('8.8.8.8', { countryCode: 'US', kind: 'srflx' })
 * // '🇺🇸 8.8.8.8 (srflx)'
 *
 * formatIpWithFlag('10.0.0.1', { kind: 'host' })
 * // '10.0.0.1 (host)'
 *
 * formatIpWithFlag(null)
 * // '—'
 */
export function formatIpWithFlag(
  ip: string | null | undefined,
  options: FormatIpWithFlagOptions = {},
): string {
  if (ip == null || ip.trim().length === 0) return '—'
  const trimmed = ip.trim()
  const flag = countryCodeToFlag(options.countryCode ?? null)
  const kind = options.kind?.trim()
  const body = kind ? `${trimmed} (${kind})` : trimmed
  return flag ? `${flag} ${body}` : body
}

/** Default GeoIP endpoint — HTTPS JSON, `country_code` field. */
export const DEFAULT_GEOIP_URL = (ip: string) =>
  `https://ipwho.is/${encodeURIComponent(ip)}?fields=success,country_code`

/**
 * Look up an ISO country code for a public IP.
 *
 * Returns `null` for non-candidates, network errors, or unsuccessful responses.
 * `fetchImpl` is injectable so tests never touch the network.
 */
export async function lookupCountryCode(
  ip: string,
  fetchImpl: typeof fetch = fetch,
  urlForIp: (ip: string) => string = DEFAULT_GEOIP_URL,
): Promise<string | null> {
  if (!isGeoLookupCandidate(ip)) return null
  try {
    const response = await fetchImpl(urlForIp(ip.trim()), {
      method: 'GET',
      headers: { Accept: 'application/json' },
    })
    if (!response.ok) return null
    const body = (await response.json()) as {
      success?: boolean
      country_code?: string
    }
    if (body.success === false) return null
    return normalizeCountryCode(body.country_code ?? null)
  } catch {
    return null
  }
}
