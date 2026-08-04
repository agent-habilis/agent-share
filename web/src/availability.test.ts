import { describe, expect, test } from 'bun:test'

import {
  decodeServing,
  missingSlots,
  peerAvailability,
  slotCoverage,
} from './availability.ts'

describe('decodeServing', () => {
  test('a complete peer expands to every slot', () => {
    expect(decodeServing('*', 4)).toEqual([0, 1, 2, 3])
  })

  test('runs and singles decode together', () => {
    expect(decodeServing('0-3,7,9-10', 20)).toEqual([0, 1, 2, 3, 7, 9, 10])
  })

  test('the result is sorted and deduplicated', () => {
    expect(decodeServing('5,1-2,5,0', 10)).toEqual([0, 1, 2, 5])
  })

  // Matches the Rust decoder's tolerance: one unreadable run must cost that
  // run, not the peer's whole availability.
  test('a malformed run is skipped rather than fatal', () => {
    expect(decodeServing('0-2,nonsense,7,9-x,12', 100)).toEqual([0, 1, 2, 7, 12])
    expect(decodeServing('5-1,3', 100)).toEqual([3])
  })

  test('an empty field decodes to nothing', () => {
    expect(decodeServing('', 10)).toEqual([])
  })
})

describe('peerAvailability', () => {
  // Absent is "cannot vouch", not "holds nothing". Rendering an empty row for a
  // peer that simply has not said would claim something it never claimed.
  test('a missing field is unknown, not empty', () => {
    const peer = peerAvailability('abc', null, 'tree1', 8)
    expect(peer.unknown).toBe(true)
    expect(peer.complete).toBe(false)
    expect(peer.held).toEqual([])
  })

  test('a star is complete', () => {
    const peer = peerAvailability('abc', '*', 'tree1', 3)
    expect(peer.complete).toBe(true)
    expect(peer.unknown).toBe(false)
    expect(peer.held).toEqual([0, 1, 2])
  })

  test('a partial peer keeps its ranges and its tree', () => {
    const peer = peerAvailability('abc', '0-1,4', 'tree1', 8)
    expect(peer.held).toEqual([0, 1, 4])
    expect(peer.tree).toBe('tree1')
    expect(peer.complete).toBe(false)
  })
})

describe('slotCoverage', () => {
  test('counts how many peers hold each slot', () => {
    const peers = [
      peerAvailability('a', '0-1', 't', 4),
      peerAvailability('b', '1-3', 't', 4),
    ]
    expect(slotCoverage(peers, 4)).toEqual([1, 2, 1, 1])
  })

  // A peer that has not said anything must not be counted as holding nothing;
  // it is simply not evidence either way.
  test('an unknown peer contributes nothing', () => {
    const peers = [peerAvailability('a', null, 't', 3)]
    expect(slotCoverage(peers, 3)).toEqual([0, 0, 0])
  })
})

describe('missingSlots', () => {
  // The single most useful thing this view can say once the origin is gone:
  // these parts of the share are lost unless somebody who has them reappears.
  test('reports slots no visible peer holds', () => {
    const peers = [
      peerAvailability('a', '0', 't', 4),
      peerAvailability('b', '2', 't', 4),
    ]
    expect(missingSlots(peers, 4)).toEqual([1, 3])
  })

  test('a complete peer leaves nothing missing', () => {
    expect(missingSlots([peerAvailability('a', '*', 't', 5)], 5)).toEqual([])
  })
})
