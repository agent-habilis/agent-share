import { describe, expect, test } from 'bun:test'

import { sortPeers } from './peers.ts'

function peer(role: string, id: string) {
  return { role, id }
}

const ids = (rows: { id: string }[]) => rows.map((row) => row.id)

describe('sortPeers', () => {
  test('self and the producer are pinned to the top', () => {
    const sorted = sortPeers([
      peer('gossip', 'aaa'),
      peer('producer', 'zzz'),
      peer('self', 'mmm'),
    ])
    expect(ids(sorted)).toEqual(['mmm', 'zzz', 'aaa'])
  })

  // The bug this exists to prevent. Both sources in the wasm client are
  // hash-ordered and rebuilt on every poll, so the same peers arrive in a
  // different sequence each second. The rendered order must not follow.
  test('the same peers sort the same however they arrive', () => {
    const first = sortPeers([
      peer('self', 's'),
      peer('producer', 'p'),
      peer('gossip', 'd814'),
      peer('direct', '3b74'),
      peer('gossip', '6388'),
    ])
    const second = sortPeers([
      peer('gossip', '6388'),
      peer('producer', 'p'),
      peer('direct', '3b74'),
      peer('self', 's'),
      peer('gossip', 'd814'),
    ])
    expect(ids(first)).toEqual(ids(second))
    expect(ids(first)).toEqual(['s', 'p', '3b74', '6388', 'd814'])
  })

  // Ordered by id rather than by role, so a peer that opens a data channel
  // changes its flags and not its position.
  test('a peer that gains a direct channel keeps its place', () => {
    const before = sortPeers([
      peer('self', 's'),
      peer('gossip', 'bbb'),
      peer('direct', 'ccc'),
      peer('gossip', 'aaa'),
    ])
    const after = sortPeers([
      peer('self', 's'),
      peer('direct', 'bbb'), // was gossip-only
      peer('direct', 'ccc'),
      peer('gossip', 'aaa'),
    ])
    expect(ids(before)).toEqual(ids(after))
    expect(ids(before)).toEqual(['s', 'aaa', 'bbb', 'ccc'])
  })

  test('a new peer lands in id order rather than at the end', () => {
    const sorted = sortPeers([
      peer('self', 's'),
      peer('gossip', 'ccc'),
      peer('gossip', 'aaa'),
      peer('gossip', 'bbb'), // joined last
    ])
    expect(ids(sorted)).toEqual(['s', 'aaa', 'bbb', 'ccc'])
  })

  test('the input is not mutated', () => {
    const input = [peer('gossip', 'bbb'), peer('self', 'aaa')]
    const snapshot = ids(input)
    sortPeers(input)
    expect(ids(input)).toEqual(snapshot)
  })

  test('an empty list stays empty', () => {
    expect(sortPeers([])).toEqual([])
  })
})
