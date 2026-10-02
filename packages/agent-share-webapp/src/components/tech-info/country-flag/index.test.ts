import { describe, expect, test } from 'bun:test'

import {
  __setCountryTableForTests,
  countryCodeToFlag,
  formatIpWithFlag,
  isGeoLookupCandidate,
  isLocalAddress,
  lookupCountryCode,
  normalizeCountryCode,
} from './index.ts'

// ---------------------------------------------------------------------------
// normalizeCountryCode
// ---------------------------------------------------------------------------

describe('normalizeCountryCode', () => {
  test('uppercases a lowercase alpha-2 code', () => {
    expect(normalizeCountryCode('us')).toBe('US')
    expect(normalizeCountryCode('br')).toBe('BR')
  })

  test('trims whitespace', () => {
    expect(normalizeCountryCode('  de  ')).toBe('DE')
  })

  test('maps UK to GB (ISO uses GB for the United Kingdom)', () => {
    expect(normalizeCountryCode('uk')).toBe('GB')
    expect(normalizeCountryCode('UK')).toBe('GB')
  })

  test('rejects nullish and empty', () => {
    expect(normalizeCountryCode(null)).toBeNull()
    expect(normalizeCountryCode(undefined)).toBeNull()
    expect(normalizeCountryCode('')).toBeNull()
    expect(normalizeCountryCode('   ')).toBeNull()
  })

  test('rejects wrong length', () => {
    expect(normalizeCountryCode('U')).toBeNull()
    expect(normalizeCountryCode('USA')).toBeNull()
    expect(normalizeCountryCode('United States')).toBeNull()
  })

  test('rejects non-letters', () => {
    expect(normalizeCountryCode('U1')).toBeNull()
    expect(normalizeCountryCode('1S')).toBeNull()
    expect(normalizeCountryCode('12')).toBeNull()
    expect(normalizeCountryCode('u$')).toBeNull()
  })

  test('accepts user-assigned codes (still two letters)', () => {
    expect(normalizeCountryCode('XX')).toBe('XX')
    expect(normalizeCountryCode('ZZ')).toBe('ZZ')
  })
})

// ---------------------------------------------------------------------------
// countryCodeToFlag
// ---------------------------------------------------------------------------

describe('countryCodeToFlag', () => {
  test('builds well-known flags', () => {
    expect(countryCodeToFlag('US')).toBe('🇺🇸')
    expect(countryCodeToFlag('BR')).toBe('🇧🇷')
    expect(countryCodeToFlag('JP')).toBe('🇯🇵')
    expect(countryCodeToFlag('DE')).toBe('🇩🇪')
    expect(countryCodeToFlag('FR')).toBe('🇫🇷')
  })

  test('is case-insensitive and trims', () => {
    expect(countryCodeToFlag('us')).toBe('🇺🇸')
    expect(countryCodeToFlag('  jp ')).toBe('🇯🇵')
  })

  test('UK becomes the GB flag', () => {
    expect(countryCodeToFlag('UK')).toBe('🇬🇧')
    expect(countryCodeToFlag('GB')).toBe('🇬🇧')
  })

  test('each flag is exactly two regional-indicator code points', () => {
    const flag = countryCodeToFlag('CA')
    expect(flag).not.toBeNull()
    const points = [...flag!]
    expect(points).toHaveLength(2)
    // Regional Indicator range: U+1F1E6..U+1F1FF
    for (const ch of points) {
      const cp = ch.codePointAt(0)!
      expect(cp).toBeGreaterThanOrEqual(0x1f1e6)
      expect(cp).toBeLessThanOrEqual(0x1f1ff)
    }
  })

  test('A maps to the first regional indicator, Z to the last', () => {
    const aa = countryCodeToFlag('AA')!
    const zz = countryCodeToFlag('ZZ')!
    expect([...aa][0]!.codePointAt(0)).toBe(0x1f1e6)
    expect([...aa][1]!.codePointAt(0)).toBe(0x1f1e6)
    expect([...zz][0]!.codePointAt(0)).toBe(0x1f1ff)
    expect([...zz][1]!.codePointAt(0)).toBe(0x1f1ff)
  })

  test('returns null for invalid codes', () => {
    expect(countryCodeToFlag(null)).toBeNull()
    expect(countryCodeToFlag(undefined)).toBeNull()
    expect(countryCodeToFlag('')).toBeNull()
    expect(countryCodeToFlag('USA')).toBeNull()
    expect(countryCodeToFlag('u')).toBeNull()
    expect(countryCodeToFlag('🇺🇸')).toBeNull()
  })

  test('distinct countries produce distinct emoji', () => {
    const us = countryCodeToFlag('US')
    const ca = countryCodeToFlag('CA')
    const mx = countryCodeToFlag('MX')
    expect(us).not.toBe(ca)
    expect(ca).not.toBe(mx)
    expect(us).not.toBe(mx)
  })
})

// ---------------------------------------------------------------------------
// isGeoLookupCandidate
// ---------------------------------------------------------------------------

describe('isGeoLookupCandidate', () => {
  test('accepts public IPv4', () => {
    expect(isGeoLookupCandidate('8.8.8.8')).toBe(true)
    expect(isGeoLookupCandidate('1.1.1.1')).toBe(true)
    expect(isGeoLookupCandidate('203.0.114.1')).toBe(true)
  })

  test('rejects private and special IPv4', () => {
    expect(isGeoLookupCandidate('10.0.0.1')).toBe(false)
    expect(isGeoLookupCandidate('127.0.0.1')).toBe(false)
    expect(isGeoLookupCandidate('192.168.1.1')).toBe(false)
    expect(isGeoLookupCandidate('172.16.0.1')).toBe(false)
    expect(isGeoLookupCandidate('172.31.255.255')).toBe(false)
    expect(isGeoLookupCandidate('169.254.1.1')).toBe(false)
    expect(isGeoLookupCandidate('100.64.0.1')).toBe(false)
    expect(isGeoLookupCandidate('192.0.2.1')).toBe(false) // TEST-NET-1
    expect(isGeoLookupCandidate('198.51.100.1')).toBe(false)
    expect(isGeoLookupCandidate('203.0.113.1')).toBe(false)
  })

  test('172.15 and 172.32 are public (outside RFC1918 172.16/12)', () => {
    expect(isGeoLookupCandidate('172.15.0.1')).toBe(true)
    expect(isGeoLookupCandidate('172.32.0.1')).toBe(true)
  })

  test('rejects malformed IPv4', () => {
    expect(isGeoLookupCandidate('8.8.8')).toBe(false)
    expect(isGeoLookupCandidate('8.8.8.8.8')).toBe(false)
    expect(isGeoLookupCandidate('256.0.0.1')).toBe(false)
    expect(isGeoLookupCandidate('1.2.3.4a')).toBe(false)
    expect(isGeoLookupCandidate('not-an-ip')).toBe(false)
  })

  test('rejects mDNS and empty', () => {
    expect(isGeoLookupCandidate('abc.local')).toBe(false)
    expect(isGeoLookupCandidate('foo.bar.local')).toBe(false)
    expect(isGeoLookupCandidate('')).toBe(false)
    expect(isGeoLookupCandidate(null)).toBe(false)
    expect(isGeoLookupCandidate(undefined)).toBe(false)
    expect(isGeoLookupCandidate('   ')).toBe(false)
  })

  test('IPv6: accepts global, rejects loopback / link-local / ULA', () => {
    expect(isGeoLookupCandidate('2001:4860:4860::8888')).toBe(true)
    expect(isGeoLookupCandidate('::1')).toBe(false)
    expect(isGeoLookupCandidate('fe80::1')).toBe(false)
    expect(isGeoLookupCandidate('fc00::1')).toBe(false)
    expect(isGeoLookupCandidate('fd12:3456:789a::1')).toBe(false)
  })

  test('IPv4-mapped IPv6 follows the embedded v4 rules', () => {
    expect(isGeoLookupCandidate('::ffff:8.8.8.8')).toBe(true)
    expect(isGeoLookupCandidate('::ffff:10.0.0.1')).toBe(false)
    expect(isGeoLookupCandidate('::ffff:192.168.0.1')).toBe(false)
  })

  test('rejects zone-indexed addresses', () => {
    expect(isGeoLookupCandidate('fe80::1%eth0')).toBe(false)
  })
})

// ---------------------------------------------------------------------------
// formatIpWithFlag
// ---------------------------------------------------------------------------

describe('formatIpWithFlag', () => {
  test('prefixes a flag when a country code is provided', () => {
    expect(formatIpWithFlag('8.8.8.8', { countryCode: 'US' })).toBe('🇺🇸 8.8.8.8')
  })

  test('includes kind in parentheses after the IP', () => {
    expect(formatIpWithFlag('8.8.8.8', { countryCode: 'US', kind: 'srflx' })).toBe(
      '🇺🇸 8.8.8.8 (srflx)',
    )
  })

  test('omits the flag when country is missing or invalid', () => {
    expect(formatIpWithFlag('8.8.8.8', { kind: 'host' })).toBe('8.8.8.8 (host)')
    expect(formatIpWithFlag('8.8.8.8', { countryCode: 'USA' })).toBe('8.8.8.8')
    expect(formatIpWithFlag('8.8.8.8')).toBe('8.8.8.8')
  })

  test('returns an em dash for nullish / empty IP', () => {
    expect(formatIpWithFlag(null)).toBe('—')
    expect(formatIpWithFlag(undefined)).toBe('—')
    expect(formatIpWithFlag('')).toBe('—')
    expect(formatIpWithFlag('  ')).toBe('—')
  })

  test('trims the IP and kind', () => {
    expect(formatIpWithFlag('  1.1.1.1  ', { countryCode: 'AU', kind: ' relay ' })).toBe(
      '🇦🇺 1.1.1.1 (relay)',
    )
  })

  test('marks local addresses instead of leaving them bare', () => {
    // An mDNS name resolves on this network and nowhere else, so it gets the
    // local marker rather than a flag — the point is that it reads as
    // deliberately local, not as a lookup that failed.
    expect(formatIpWithFlag('abc.local', { kind: 'mdns' })).toBe('🏠 abc.local (mdns)')
    expect(formatIpWithFlag('192.168.1.5', { kind: 'host' })).toBe('🏠 192.168.1.5 (host)')
    expect(formatIpWithFlag('127.0.0.1')).toBe('🏠 127.0.0.1')
  })

  test('a country flag still wins for public addresses', () => {
    expect(formatIpWithFlag('200.0.0.1', { countryCode: 'BR', kind: 'srflx' })).toBe(
      '🇧🇷 200.0.0.1 (srflx)',
    )
  })

  test('UK alias yields the GB flag in the prefix', () => {
    expect(formatIpWithFlag('9.9.9.9', { countryCode: 'uk' })).toBe('🇬🇧 9.9.9.9')
  })
})

// ---------------------------------------------------------------------------
// lookupCountryCode (offline table)
// ---------------------------------------------------------------------------

/** A tiny stand-in table: block index -> country index, codes indexed from 1. */
function tableWith(entries: Array<[number, number]>, codes: string[]) {
  const blocks = new Uint8Array(1 << 20)
  for (const [block, index] of entries) blocks[block] = index
  return { codes, blocks }
}

/** `a.b.c.d` to its /20 block index — the same shift the lookup uses. */
function block(ip: string): number {
  const [a, b, c, d] = ip.split('.').map(Number) as [number, number, number, number]
  return (((a << 24) | (b << 16) | (c << 8) | d) >>> 12) >>> 0
}

describe('lookupCountryCode', () => {
  test('resolves a public address through the table', async () => {
    __setCountryTableForTests(tableWith([[block('200.0.0.1'), 2]], ['AR', 'BR']))
    expect(await lookupCountryCode('200.0.0.1')).toBe('BR')
  })

  test('never consults the table for private / mDNS addresses', async () => {
    __setCountryTableForTests(tableWith([[block('10.0.0.1'), 2]], ['AR', 'BR']))
    // Even with an entry sitting at that block, a private address is refused
    // before the lookup — the table is only meaningful for public space.
    expect(await lookupCountryCode('10.0.0.1')).toBeNull()
    expect(await lookupCountryCode('foo.local')).toBeNull()
    expect(await lookupCountryCode('192.168.1.5')).toBeNull()
  })

  test('returns null for a block with no recorded allocation', async () => {
    __setCountryTableForTests(tableWith([], ['AR', 'BR']))
    expect(await lookupCountryCode('8.8.8.8')).toBeNull()
  })

  test('returns null when the table cannot be loaded', async () => {
    __setCountryTableForTests(null)
    expect(await lookupCountryCode('8.8.8.8')).toBeNull()
  })

  test('resolves an IPv4-mapped IPv6 address through the v4 table', async () => {
    __setCountryTableForTests(tableWith([[block('200.0.0.1'), 2]], ['AR', 'BR']))
    expect(await lookupCountryCode('::ffff:200.0.0.1')).toBe('BR')
  })

  test('returns null for real IPv6 — the table is v4 only', async () => {
    __setCountryTableForTests(tableWith([[0, 2]], ['AR', 'BR']))
    expect(await lookupCountryCode('2001:4860:4860::8888')).toBeNull()
  })
})

describe('isLocalAddress', () => {
  test('recognises this-machine and this-network addresses', () => {
    for (const ip of ['127.0.0.1', '10.0.0.1', '192.168.1.5', '172.16.0.1', '100.64.0.1', 'a.local']) {
      expect(isLocalAddress(ip)).toBe(true)
    }
  })

  test('public addresses are not local', () => {
    for (const ip of ['8.8.8.8', '200.0.0.1']) expect(isLocalAddress(ip)).toBe(false)
  })

  test('empty is neither', () => {
    expect(isLocalAddress('')).toBe(false)
    expect(isLocalAddress(null)).toBe(false)
  })
})
