import { describe, expect, test } from 'bun:test'

import { detectRuntime } from './peerCard.ts'

describe('detectRuntime', () => {
  test('safari desktop is safari, not chrome', () => {
    const ua =
      'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15'
    expect(detectRuntime(ua)).toBe('safari')
  })

  test('chrome desktop is chrome despite Safari token', () => {
    const ua =
      'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36'
    expect(detectRuntime(ua)).toBe('chrome')
  })
})
