/**
 * Generate the offline IPv4 → country table served to the Info pane.
 *
 * Run: `bun run scripts/build-ip-country.ts` (from the repo root). Network-only
 * at build time; the app itself never calls out.
 *
 * # Why this exists
 *
 * The Info pane used to resolve peer IPs through `ipwho.is`, which meant every
 * peer address a tab saw was handed to a third party. For a project whose point
 * is keeping infrastructure out of the data plane, leaking the addresses to a
 * geo API is the same mistake one layer up.
 *
 * # Why /20
 *
 * Measured against RIPE's delegation file, dominant-country-per-block, weighted
 * by addresses rather than blocks:
 *
 * ```text
 * prefix   blocks    accuracy      raw     gzip
 * /16      14,244      85.76%      64K     8.8K
 * /18      53,742      91.99%     256K    19.5K
 * /20     210,289      96.25%    1024K    41.1K   <- chosen
 * /22     835,932      99.38%    4096K    84.0K
 * ```
 *
 * /20 is the knee: /18 misattributes one peer in twelve, and a confidently
 * wrong flag is worse than none; /22 costs a 4 MB array in a tab for what is
 * decoration. Exact ranges would be ~278 KB gzipped and need a binary search —
 * this is one array index.
 *
 * # Why the RIRs rather than a library
 *
 * `ip3country` is the small-bundle benchmark (<300 KB) but compiles the table
 * *into* the JS bundle and carries an IP2Location LITE attribution
 * requirement. The RIR delegation files are the authoritative upstream both
 * approaches derive from, freely redistributable, and cost the bundle nothing
 * because the output ships as a lazily-fetched asset.
 */

const RIRS = [
  'https://ftp.ripe.net/pub/stats/ripencc/delegated-ripencc-extended-latest',
  'https://ftp.arin.net/pub/stats/arin/delegated-arin-extended-latest',
  'https://ftp.apnic.net/stats/apnic/delegated-apnic-extended-latest',
  'https://ftp.lacnic.net/pub/stats/lacnic/delegated-lacnic-extended-latest',
  'https://ftp.afrinic.net/pub/stats/afrinic/delegated-afrinic-extended-latest',
]

/** Block size. 2^20 entries, one byte each. */
const PREFIX_BITS = 20
const SHIFT = 32 - PREFIX_BITS
const BLOCKS = 1 << PREFIX_BITS

const OUT = 'packages/agent-share-web/src/components/tech-info/country-flag/table.ts'

function toInt(dotted: string): number | null {
  const parts = dotted.split('.')
  if (parts.length !== 4) return null
  let value = 0
  for (const part of parts) {
    const octet = Number(part)
    if (!Number.isInteger(octet) || octet < 0 || octet > 255) return null
    value = value * 256 + octet
  }
  return value
}

/** `blockIndex -> countryCode -> address count`, so ties resolve by size. */
const weights = new Map<number, Map<string, number>>()

for (const url of RIRS) {
  process.stderr.write(`fetching ${new URL(url).host}… `)
  const response = await fetch(url)
  if (!response.ok) throw new Error(`${url}: HTTP ${response.status}`)
  const text = await response.text()
  let rows = 0
  for (const line of text.split('\n')) {
    // registry|cc|type|start|value|date|status|...
    const f = line.split('|')
    if (f.length < 7) continue
    if (f[2] !== 'ipv4' || !f[1]) continue
    // `available` / `reserved` rows carry no country worth trusting.
    if (f[6] !== 'allocated' && f[6] !== 'assigned') continue
    const start = toInt(f[3]!)
    const count = Number(f[4])
    if (start == null || !Number.isInteger(count) || count <= 0) continue
    const end = start + count - 1
    rows += 1
    for (let block = start >>> SHIFT; block <= end >>> SHIFT; block += 1) {
      const lo = Math.max(start, block * 2 ** SHIFT)
      const hi = Math.min(end, block * 2 ** SHIFT + 2 ** SHIFT - 1)
      let byCc = weights.get(block)
      if (!byCc) {
        byCc = new Map()
        weights.set(block, byCc)
      }
      byCc.set(f[1]!, (byCc.get(f[1]!) ?? 0) + (hi - lo + 1))
    }
  }
  process.stderr.write(`${rows} ipv4 rows\n`)
}

// Country codes, sorted so the index is stable across regenerations — a
// reshuffled table would silently repoint every block.
const codes = [...new Set([...weights.values()].flatMap((m) => [...m.keys()]))].sort()
if (codes.length > 254) throw new Error(`${codes.length} countries exceeds the one-byte index`)
const codeIndex = new Map(codes.map((cc, i) => [cc, i + 1])) // 0 = unknown

const table = new Uint8Array(BLOCKS)
let covered = 0
let attributed = 0
let total = 0
for (const [block, byCc] of weights) {
  let best = ''
  let bestCount = -1
  let sum = 0
  for (const [cc, count] of byCc) {
    sum += count
    if (count > bestCount) {
      best = cc
      bestCount = count
    }
  }
  table[block] = codeIndex.get(best)!
  covered += 1
  attributed += bestCount
  total += sum
}

// Run-length encode before base64. The array is 1 MB of long identical runs,
// so RLE takes it to ~97 KB before encoding — plain base64 of the raw array
// would be 1.4 MB of source for no benefit the gzip layer does not already
// give (measured: 68.5 KB vs 83.8 KB gzipped).
function varint(value: number): number[] {
  const out: number[] = []
  let v = value
  for (;;) {
    const byte = v & 0x7f
    v >>>= 7
    out.push(v ? byte | 0x80 : byte)
    if (!v) break
  }
  return out
}

const rle: number[] = []
let runs = 0
for (let i = 0; i < table.length; ) {
  let j = i
  while (j + 1 < table.length && table[j + 1] === table[i]) j += 1
  rle.push(...varint(j - i + 1), table[i]!)
  runs += 1
  i = j + 1
}
const encoded = btoa(String.fromCharCode(...new Uint8Array(rle)))

const source = `// @generated by scripts/build-ip-country.ts — do not edit.
//
// IPv4 → country, one entry per /${PREFIX_BITS} block, from the five RIR
// delegation files. ${covered.toLocaleString()} blocks, ${codes.length} countries,
// ${((attributed * 100) / total).toFixed(2)}% accurate address-weighted.
//
// Imported dynamically so it lands in its own chunk: the Info pane is the only
// caller, and a peer list is the only thing that needs a country.

/** Concatenated ISO 3166-1 alpha-2 codes, two chars each, index from 1. */
export const CODES = '${codes.join('')}'

/** Run-length encoded blocks: base64 of (varint runLength, countryIndex) pairs. */
export const RLE = '${encoded}'
`
await Bun.write(OUT, source)

process.stderr.write(
  `\n${covered.toLocaleString()} /${PREFIX_BITS} blocks · ${codes.length} countries · ${runs.toLocaleString()} runs\n` +
    `accuracy ${((attributed * 100) / total).toFixed(2)}% (address-weighted)\n` +
    `${OUT}: ${(source.length / 1024).toFixed(0)} KB source\n`,
)
