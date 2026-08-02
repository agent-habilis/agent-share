import { describe, expect, test } from 'bun:test'

import {
  DEFAULT_GEOIP_URL,
  countryCodeToFlag,
  formatIpWithFlag,
  isGeoLookupCandidate,
  lookupCountryCode,
  normalizeCountryCode,
} from './countryFlag.ts'

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

  test('works for mDNS names without a flag', () => {
    expect(formatIpWithFlag('abc.local', { kind: 'mdns' })).toBe('abc.local (mdns)')
  })

  test('UK alias yields the GB flag in the prefix', () => {
    expect(formatIpWithFlag('9.9.9.9', { countryCode: 'uk' })).toBe('🇬🇧 9.9.9.9')
  })
})

// ---------------------------------------------------------------------------
// lookupCountryCode (mocked fetch)
// ---------------------------------------------------------------------------

describe('lookupCountryCode', () => {
  test('returns null without calling fetch for private / mDNS addresses', async () => {
    let called = 0
    const fetchImpl = (async () => {
      called += 1
      return new Response('{}')
    }) as typeof fetch
    expect(await lookupCountryCode('10.0.0.1', fetchImpl)).toBeNull()
    expect(await lookupCountryCode('foo.local', fetchImpl)).toBeNull()
    expect(called).toBe(0)
  })

  test('parses a successful ipwho.is-shaped body', async () => {
    const fetchImpl = (async () =>
      new Response(JSON.stringify({ success: true, country_code: 'br' }), {
        status: 200,
        headers: { 'Content-Type': 'application/json' },
      })) as typeof fetch
    expect(await lookupCountryCode('200.0.0.1', fetchImpl)).toBe('BR')
  })

  test('returns null when success is false', async () => {
    const fetchImpl = (async () =>
      new Response(JSON.stringify({ success: false, country_code: 'US' }), {
        status: 200,
      })) as typeof fetch
    expect(await lookupCountryCode('8.8.8.8', fetchImpl)).toBeNull()
  })

  test('returns null on HTTP error', async () => {
    const fetchImpl = (async () => new Response('nope', { status: 500 })) as typeof fetch
    expect(await lookupCountryCode('8.8.8.8', fetchImpl)).toBeNull()
  })

  test('returns null when fetch throws', async () => {
    const fetchImpl = (async () => {
      throw new Error('network down')
    }) as typeof fetch
    expect(await lookupCountryCode('8.8.8.8', fetchImpl)).toBeNull()
  })

  test('returns null when country_code is missing or invalid', async () => {
    const missing = (async () =>
      new Response(JSON.stringify({ success: true }), { status: 200 })) as typeof fetch
    const bad = (async () =>
      new Response(JSON.stringify({ success: true, country_code: 'USA' }), {
        status: 200,
      })) as typeof fetch
    expect(await lookupCountryCode('8.8.8.8', missing)).toBeNull()
    expect(await lookupCountryCode('8.8.8.8', bad)).toBeNull()
  })

  test('DEFAULT_GEOIP_URL encodes the IP', () => {
    expect(DEFAULT_GEOIP_URL('8.8.8.8')).toContain('8.8.8.8')
    expect(DEFAULT_GEOIP_URL('2001:db8::1')).toContain(encodeURIComponent('2001:db8::1'))
  })

  test('uses the provided urlForIp builder', async () => {
    const seen: string[] = []
    const fetchImpl = (async (input: RequestInfo | URL) => {
      seen.push(String(input))
      return new Response(JSON.stringify({ success: true, country_code: 'DE' }), {
        status: 200,
      })
    }) as typeof fetch
    const code = await lookupCountryCode('9.9.9.9', fetchImpl, (ip) => `https://example.test/${ip}`)
    expect(code).toBe('DE')
    expect(seen).toEqual(['https://example.test/9.9.9.9'])
  })
})
