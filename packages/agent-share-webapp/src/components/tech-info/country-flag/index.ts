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

/**
 * Whether `ip` is on this machine or this network — loopback, RFC1918, CGNAT,
 * link-local, or an mDNS name.
 *
 * The inverse of {@link isGeoLookupCandidate} for addresses that *are* valid,
 * so a LAN peer reads as deliberately local rather than as a failed lookup.
 * With direct pairing now the goal, "this peer is on your network" is the
 * good outcome, and it should not look like missing data.
 */
export function isLocalAddress(ip: string | null | undefined): boolean {
  if (ip == null) return false
  const value = ip.trim()
  if (value.length === 0) return false
  return !isGeoLookupCandidate(value)
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
  // No address but a known candidate type: say what we know. Chrome blanks the
  // local candidate's address on purpose, so `—` alone would throw away the
  // half that actually tells you whether the peer is direct.
  if (ip == null || ip.trim().length === 0) {
    const only = options.kind?.trim()
    return only ? `— (${only})` : '—'
  }
  const trimmed = ip.trim()
  const kind = options.kind?.trim()
  const body = kind ? `${trimmed} (${kind})` : trimmed
  // A local address has no country and never will; mark it so rather than
  // leaving it bare next to peers that carry a flag.
  if (isLocalAddress(trimmed)) return `\u{1F3E0} ${body}`
  const flag = countryCodeToFlag(options.countryCode ?? null)
  return flag ? `${flag} ${body}` : body
}

/** Resolved table, or `null` once a load has failed. */
let tablePromise: Promise<CountryTable | null> | null = null

interface CountryTable {
  /** ISO codes, indexed from 1; 0 means "no allocation recorded". */
  codes: string[]
  /** One country index per /20 block, indexed by `ip >>> 12`. */
  blocks: Uint8Array
}

/**
 * Load and expand the table, at most once per page.
 *
 * **Lazy on purpose.** `import()` rather than a static import, so the ~130 KB
 * of generated source lands in its own chunk that is fetched only when
 * something actually asks for a country — in practice only when the Info pane
 * renders a public peer address. A failed load is remembered as `null` so a
 * broken chunk costs one attempt, not one per peer per second.
 */
async function loadTable(): Promise<CountryTable | null> {
  try {
    const { CODES, RLE } = await import('./table.ts')
    const codes: string[] = []
    for (let i = 0; i < CODES.length; i += 2) codes.push(CODES.slice(i, i + 2))

    const packed = atob(RLE)
    const blocks = new Uint8Array(1 << 20)
    let at = 0
    let out = 0
    while (at < packed.length && out < blocks.length) {
      // varint run length, then the country index it repeats
      let run = 0
      let shift = 0
      for (;;) {
        const byte = packed.charCodeAt(at)
        at += 1
        run |= (byte & 0x7f) << shift
        if ((byte & 0x80) === 0) break
        shift += 7
      }
      const code = packed.charCodeAt(at)
      at += 1
      blocks.fill(code, out, Math.min(out + run, blocks.length))
      out += run
    }
    return { codes, blocks }
  } catch {
    return null
  }
}

/** IPv4 dotted-quad to a 32-bit number, or `null` if it is not one. */
function ipv4ToInt(ip: string): number | null {
  const parts = ip.split('.')
  if (parts.length !== 4) return null
  let value = 0
  for (const part of parts) {
    if (!/^\d{1,3}$/.test(part)) return null
    const octet = Number(part)
    if (octet > 255) return null
    value = value * 256 + octet
  }
  return value >>> 0
}

/**
 * Look up an ISO country code for a public IP, entirely offline.
 *
 * Returns `null` for non-candidates, IPv6 (the table is v4-only), and blocks
 * with no recorded allocation.
 *
 * This replaced a call to a third-party GeoIP API. That call handed every peer
 * address a tab observed to someone else — the same mistake, one layer up, as
 * relaying file bytes through infrastructure we do not run.
 */
export async function lookupCountryCode(ip: string): Promise<string | null> {
  if (!isGeoLookupCandidate(ip)) return null
  const value = ip.trim()
  // The table is IPv4; an IPv4-mapped v6 address still resolves through it.
  const v4 = value.toLowerCase().startsWith('::ffff:')
    ? value.slice('::ffff:'.length)
    : value
  const address = ipv4ToInt(v4)
  if (address == null) return null

  tablePromise ??= loadTable()
  const table = await tablePromise
  if (table == null) return null

  const index = table.blocks[address >>> 12] ?? 0
  if (index === 0) return null
  return normalizeCountryCode(table.codes[index - 1] ?? null)
}

/** Test seam: install a table directly and skip the dynamic import. */
export function __setCountryTableForTests(table: CountryTable | null): void {
  tablePromise = Promise.resolve(table)
}
