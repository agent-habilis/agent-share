import { beforeEach, describe, expect, test } from 'bun:test'

import {
  agentActivity,
  beginToolCall,
  markToolsRegistered,
  resetAgentActivity,
  subscribeAgentActivity,
} from './activity.ts'

beforeEach(resetAgentActivity)

describe('what the page can know about an agent', () => {
  /**
   * The claim the UI must never make. WebMCP has no agent-session concept and
   * never tells a page that something connected, so publishing tools is not
   * evidence of anything — a tab nobody has found looks identical.
   */
  test('publishing tools on its own is not evidence of an agent', () => {
    markToolsRegistered(['shareRead', 'shareList'])

    expect(agentActivity().registered).toEqual(['shareRead', 'shareList'])
    expect(agentActivity().calls).toBe(0)
    expect(agentActivity().lastAt).toBe(0)
  })

  test('an invocation is', () => {
    beginToolCall('shareRead')

    expect(agentActivity().calls).toBe(1)
    expect(agentActivity().lastTool).toBe('shareRead')
    expect(agentActivity().lastAt).toBeGreaterThan(0)
  })
})

describe('in-flight tracking', () => {
  // Counted at the start, so a long shareSync shows as "controlling" while it
  // runs rather than only once it has finished.
  test('a call is in flight from the moment it starts', () => {
    beginToolCall('shareSync')

    expect(agentActivity().inFlight).toBe(1)
  })

  test('ending a call clears it', () => {
    const end = beginToolCall('shareSync')
    end()

    expect(agentActivity().inFlight).toBe(0)
    expect(agentActivity().calls).toBe(1)
  })

  test('overlapping calls are counted together', () => {
    const first = beginToolCall('shareRead')
    beginToolCall('shareList')

    expect(agentActivity().inFlight).toBe(2)
    first()
    expect(agentActivity().inFlight).toBe(1)
  })

  // The wrapper calls `end` in a `finally`, and a retry or double-dispose must
  // not drive the counter below zero and leave the badge stuck dark.
  test('ending twice is harmless', () => {
    const end = beginToolCall('shareRead')
    end()
    end()

    expect(agentActivity().inFlight).toBe(0)
  })

  test('finishing a call still counts as recent activity', () => {
    const end = beginToolCall('shareRead')
    const started = agentActivity().lastAt
    end()

    expect(agentActivity().lastAt).toBeGreaterThanOrEqual(started)
  })
})

describe('subscribers', () => {
  test('are told on every change', () => {
    const seen: number[] = []
    const stop = subscribeAgentActivity((activity) => seen.push(activity.calls))

    beginToolCall('shareRead')()
    expect(seen.length).toBeGreaterThanOrEqual(2)
    stop()
  })

  test('stop hearing once unsubscribed', () => {
    let count = 0
    const stop = subscribeAgentActivity(() => (count += 1))
    stop()
    beginToolCall('shareRead')

    expect(count).toBe(0)
  })

  test('the snapshot is replaced, not mutated, so a signal sees a new value', () => {
    const before = agentActivity()
    beginToolCall('shareRead')

    expect(agentActivity()).not.toBe(before)
    expect(before.calls).toBe(0)
  })
})
