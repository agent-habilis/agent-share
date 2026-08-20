import { describe, expect, test } from 'bun:test'

import { isCoarse, resampleSlots } from './index.ts'
import type { SeedState } from '../../../lib/seeding/index.ts'

const from = (states: SeedState[]) => (slot: number) => states[slot] ?? 'none'

describe('resampleSlots', () => {
  test('is always exactly the requested length', () => {
    expect(resampleSlots(3, 20, () => 'full')).toHaveLength(20)
    expect(resampleSlots(5000, 20, () => 'full')).toHaveLength(20)
  })

  test('an empty share draws nothing', () => {
    expect(resampleSlots(0, 20, () => 'full')).toEqual([])
  })

  test('upsampling repeats each slot across a block', () => {
    // Two slots over four cells: the first slot owns the first half.
    expect(resampleSlots(2, 4, from(['full', 'none']))).toEqual([
      'full',
      'full',
      'none',
      'none',
    ])
  })

  test('one slot fills the whole line', () => {
    expect(resampleSlots(1, 4, () => 'full')).toEqual(['full', 'full', 'full', 'full'])
  })

  // The fold is pessimistic on purpose: this grid is how someone decides
  // whether a share is still recoverable, so a cell must never read as
  // complete when part of it is missing.
  test('downsampling only calls a cell full when every slot under it is', () => {
    const states: SeedState[] = ['full', 'full', 'full', 'none']
    expect(resampleSlots(4, 2, from(states))).toEqual(['full', 'partial'])
  })

  test('a cell over nothing held reads as none, not partial', () => {
    expect(resampleSlots(4, 2, from(['none', 'none', 'full', 'full']))).toEqual([
      'none',
      'full',
    ])
  })

  test('a partial slot makes its cell partial', () => {
    expect(resampleSlots(2, 2, from(['partial', 'full']))).toEqual(['partial', 'full'])
  })
})

describe('isCoarse', () => {
  test('is true only when one cell stands for more than one slot', () => {
    expect(isCoarse(20, 20)).toBe(false)
    expect(isCoarse(8, 20)).toBe(false)
    expect(isCoarse(21, 20)).toBe(true)
  })
})
