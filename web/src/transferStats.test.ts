import { describe, expect, test } from 'bun:test'

import {
  PEERS_CELLS,
  RATE_CELLS,
  fit,
  formatPeers,
  formatRate,
  formatSampledRate,
  laneSummary,
  peerCounts,
  type Lane,
} from './transferStats.ts'

function lane(label: string, received: number, selected = false): Lane {
  return {
    label,
    selected,
    sent: 0,
    received,
    up_bps: 0,
    down_bps: 0,
    rtt_ms: null,
  }
}

describe('peerCounts', () => {
  test('a relay-carried mount peer counts as connected', () => {
    // The producer-less topology: bytes flow from a seeder over the relay,
    // zero data channels. `00/…` here read as "disconnected" while a
    // download was visibly running.
    expect(peerCounts(5, 0, true)).toEqual({ connected: 1, known: 4 })
  })

  test('a webrtc-path mount adds nothing extra', () => {
    // That session already sits in the registry, so `direct` carries it;
    // counting it again would double the one peer.
    expect(peerCounts(2, 1, false)).toEqual({ connected: 1, known: 1 })
  })

  test('the relay peer keeps known from dropping below connected', () => {
    expect(peerCounts(0, 0, true)).toEqual({ connected: 1, known: 1 })
  })


  test('drops self from the gossip roster', () => {
    // Three on the mesh — us and two others — with one data channel open.
    expect(peerCounts(3, 1)).toEqual({ connected: 1, known: 2 })
  })

  test('a lone tab knows nobody', () => {
    expect(peerCounts(1, 0)).toEqual({ connected: 0, known: 0 })
  })

  test('a direct peer counts as known even with the mesh down', () => {
    // peers_gossip is 0 when the mesh never came up, but the producer session
    // is real — `1/0` would be nonsense.
    expect(peerCounts(0, 1)).toEqual({ connected: 1, known: 1 })
  })

  test('never goes negative', () => {
    expect(peerCounts(0, 0)).toEqual({ connected: 0, known: 0 })
  })
})

describe('formatRate', () => {
  test('rounds before formatting, so sub-KB rates are not float soup', () => {
    // No unit padding here — that belongs to the bar's fixed-width formatter.
    expect(formatRate(512.4)).toBe('512 B/s')
  })

  test('scales past a kilobyte', () => {
    expect(formatRate(1536)).toBe('1.5 KB/s')
  })

  test('reports nothing rather than zero when idle', () => {
    expect(formatRate(0)).toBe('—')
    expect(formatRate(Number.NaN)).toBe('—')
  })
})

describe('formatSampledRate', () => {
  test('a measured zero is zero, not a dash', () => {
    expect(formatSampledRate(0)).toBe('000 KB/s')
  })

  test('the scale starts at KB, so byte-scale chatter rounds away', () => {
    // The accepted cost of a two-character unit everywhere. A handful of bytes
    // a second is protocol noise, and this row is for watching transfers.
    expect(formatSampledRate(1)).toBe('000 KB/s')
    expect(formatSampledRate(400)).toBe('000 KB/s')
  })

  test('always three digits, zero-padded', () => {
    expect(formatSampledRate(12 * 1024)).toBe('012 KB/s')
    expect(formatSampledRate(512 * 1024)).toBe('512 KB/s')
  })

  test('rolls over at 1000, not 1024, so a fourth digit never appears', () => {
    expect(formatSampledRate(999 * 1024)).toBe('999 KB/s')
    expect(formatSampledRate(1000 * 1024)).toBe('001 MB/s')
    // 1023.6 KB rounds to 1024 — four digits — so it must promote instead.
    expect(formatSampledRate(1023.6 * 1024)).toBe('001 MB/s')
  })

  test('999 MB/s rolls straight to 001 GB/s', () => {
    expect(formatSampledRate(999 * 1024 * 1024)).toBe('999 MB/s')
    expect(formatSampledRate(1024 * 1024 * 1024)).toBe('001 GB/s')
  })

  test('still dashes a rate that cannot exist', () => {
    expect(formatSampledRate(-1)).toBe('—')
    expect(formatSampledRate(Number.NaN)).toBe('—')
  })

  test('never exceeds its column', () => {
    // The property the whole fixed-width scheme rests on. Sampled across every
    // decade, including the top unit, where promotion has nowhere left to go.
    for (let exponent = 0; exponent < 15; exponent += 1) {
      for (const scale of [1, 1.5, 3.7, 9.99]) {
        const text = formatSampledRate(10 ** exponent * scale)
        expect(text.length).toBeLessThanOrEqual(RATE_CELLS)
      }
    }
  })
})

describe('fit', () => {
  test('pads a short value to exactly the column width', () => {
    expect(fit('1/2', 5)).toBe('1/2  ')
  })

  test('clips a long one rather than letting it push', () => {
    expect(fit('1234.56', 6)).toBe('1234.5')
  })

  test('leaves an exact fit alone', () => {
    expect(fit('999 MB/s', 8)).toBe('999 MB/s')
  })

  test('is constant width for every rate the formatter can emit', () => {
    const widths = new Set(
      [0, 1, 999, 1000, 1048576, 1024 ** 3, 1024 ** 4].map(
        (bps) => fit(formatSampledRate(bps), RATE_CELLS).length,
      ),
    )
    expect(widths).toEqual(new Set([RATE_CELLS]))
  })
})

describe('formatPeers', () => {
  test('is connected of known, two digits a side', () => {
    expect(formatPeers({ connected: 2, known: 5 })).toBe('02/05')
    expect(formatPeers({ connected: 12, known: 34 })).toBe('12/34')
  })

  test('clamps rather than widening', () => {
    expect(formatPeers({ connected: 0, known: 1200 })).toBe('00/99')
  })

  test('is always exactly its column', () => {
    for (const known of [0, 1, 9, 10, 99, 100, 5000]) {
      expect(formatPeers({ connected: 0, known }).length).toBe(PEERS_CELLS)
    }
  })
})

describe('field widths', () => {
  /*
    What keeps the separators centred, and the reason the rate scale starts at
    KB rather than B. A value shorter than its column leaves padding on one side
    only, which adds air before the dot that follows while the dot's other
    neighbour stays put — so every formatter has to hit its width exactly, not
    merely stay under it.
  */
  test('every field formatter emits exactly its column width', () => {
    for (const bps of [0, 1, 999, 1024, 1024 ** 2, 1024 ** 3, 1024 ** 4]) {
      expect(formatSampledRate(bps).length).toBe(RATE_CELLS)
    }
    for (const known of [0, 1, 99, 5000]) {
      expect(formatPeers({ connected: 0, known }).length).toBe(PEERS_CELLS)
    }
  })
})

describe('laneSummary', () => {
  test('names each lane and marks the selected one', () => {
    expect(laneSummary([lane('webrtc', 2048, true), lane('relay', 0)])).toBe(
      'webrtc* 2.0 KB · relay 0 B',
    )
  })

  test('says so when there are no paths', () => {
    expect(laneSummary([])).toBe('no paths')
  })
})
