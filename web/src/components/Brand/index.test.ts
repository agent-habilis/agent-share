import { describe, expect, test } from 'bun:test'

import { describeBrand } from './index.tsx'
import type { AgentActivity } from '../../lib/agentTools/index.ts'

function activity(over: Partial<AgentActivity> = {}): AgentActivity {
  return {
    registered: [],
    calls: 0,
    inFlight: 0,
    lastAt: 0,
    lastTool: null,
    log: [],
    ...over,
  }
}

const NOW = 1_000_000

describe('before an agent has called anything', () => {
  /**
   * The claim the brand must never make. WebMCP has no agent-session concept
   * and never tells a page that something connected, so publishing tools is
   * not evidence of anything — a tab nobody has found looks identical.
   */
  test('publishing tools on its own leaves the name alone', () => {
    const state = describeBrand(activity({ registered: ['shareRead', 'shareList'] }), NOW)

    expect(state.green).toBe(false)
    expect(state.animated).toBe(false)
  })

  // Two different silences, and the tooltip is the only thing that can tell
  // them apart: nobody has called, versus nothing could have.
  test('the tooltip says how many tools are waiting', () => {
    const state = describeBrand(activity({ registered: ['shareRead', 'shareList'] }), NOW)

    expect(state.title).toContain('2 tools published')
  })

  test('a browser without WebMCP says so instead', () => {
    expect(describeBrand(activity(), NOW).title).toContain('no WebMCP')
  })
})

describe('once a tool has been called', () => {
  test('the name goes green', () => {
    const state = describeBrand(activity({ calls: 1, lastAt: NOW, lastTool: 'shareList' }), NOW)

    expect(state.green).toBe(true)
    expect(state.animated).toBe(true)
  })

  // The page cannot un-know that an agent was here, so the colour does not
  // decay the way the movement does.
  test('it stays green long after the shimmer stops', () => {
    const state = describeBrand(
      activity({ calls: 4, lastAt: NOW - 600_000, lastTool: 'shareList' }),
      NOW,
    )

    expect(state.green).toBe(true)
    expect(state.animated).toBe(false)
  })

  test('the shimmer lasts ten seconds', () => {
    const at = (ago: number) =>
      describeBrand(activity({ calls: 1, lastAt: NOW - ago, lastTool: 'shareList' }), NOW).animated

    expect(at(9_500)).toBe(true)
    expect(at(10_500)).toBe(false)
  })

  /**
   * Counted from the start of the call rather than the end of it. A `shareSync`
   * pulling a large share runs far longer than the window, and a clock-only
   * rule would stop moving during the one call an agent is most obviously in
   * the middle of.
   */
  test('a call still running shimmers however old it is', () => {
    const state = describeBrand(
      activity({ calls: 1, inFlight: 1, lastAt: NOW - 600_000, lastTool: 'shareSync' }),
      NOW,
    )

    expect(state.animated).toBe(true)
    expect(state.title).toContain('running shareSync right now')
  })
})

describe('what the tooltip is allowed to claim', () => {
  test('a finished call is reported in the past tense, with the count', () => {
    const state = describeBrand(
      activity({ calls: 3, lastAt: NOW - 7_000, lastTool: 'shareRead' }),
      NOW,
    )

    expect(state.title).toContain('shareRead')
    expect(state.title).toContain('3 actions')
  })

  test('"just now" reads better than a number under five seconds', () => {
    const state = describeBrand(
      activity({ calls: 1, lastAt: NOW - 1_000, lastTool: 'shareRead' }),
      NOW,
    )

    expect(state.title).toContain('just now')
  })

  // Nothing may suggest the agent is still there. The browser does not report
  // it, so the sentence has to say that out loud.
  test('an idle page admits it cannot tell whether the agent is still attached', () => {
    const state = describeBrand(
      activity({ calls: 2, lastAt: NOW - 60_000, lastTool: 'shareList' }),
      NOW,
    )

    expect(state.title).toMatch(/no longer be attached/)
  })

  test('every state has something to say on hover', () => {
    const states = [
      activity(),
      activity({ registered: ['shareList'] }),
      activity({ calls: 1, inFlight: 1, lastAt: NOW }),
      activity({ calls: 1, lastAt: NOW - 2_000 }),
      activity({ calls: 1, lastAt: NOW - 90_000 }),
    ]

    for (const one of states) {
      expect(describeBrand(one, NOW).title.length).toBeGreaterThan(20)
    }
  })
})
