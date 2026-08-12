import { describe, expect, test } from 'bun:test'

import { describeAgent } from './index.tsx'
import type { AgentActivity } from '../../lib/agentTools/index.ts'

const NOW = 1_700_000_000_000

function activity(over: Partial<AgentActivity> = {}): AgentActivity {
  return { registered: [], calls: 0, inFlight: 0, lastAt: 0, lastTool: null, ...over }
}

/**
 * The badge is the one place the app makes a claim about someone it cannot see.
 * WebMCP never tells a page that an agent connected, so every wording here is
 * chosen to stay inside what a tool invocation actually proves.
 */
describe('what the badge is allowed to say', () => {
  test('nothing at all before any tool has been called', () => {
    expect(describeAgent(activity(), NOW)).toBeNull()
  })

  // Publishing tools proves only that the browser supports WebMCP.
  test('nothing merely because tools are published', () => {
    expect(describeAgent(activity({ registered: ['shareRead', 'shareList'] }), NOW)).toBeNull()
  })

  test('"controlling" only while a call is actually running', () => {
    const described = describeAgent(
      activity({ calls: 3, inFlight: 1, lastAt: NOW, lastTool: 'shareSync' }),
      NOW,
    )

    expect(described?.label).toBe('agent controlling')
    expect(described?.tone).toBe('accent')
    expect(described?.title).toContain('shareSync')
  })

  test('"active" just after a call finishes', () => {
    const described = describeAgent(
      activity({ calls: 1, inFlight: 0, lastAt: NOW - 2000, lastTool: 'shareRead' }),
      NOW,
    )

    expect(described?.label).toBe('agent active')
    expect(described?.title).toContain('just now')
  })

  test('an older call inside the window is dated rather than called recent', () => {
    const described = describeAgent(
      activity({ calls: 2, inFlight: 0, lastAt: NOW - 9000, lastTool: 'shareList' }),
      NOW,
    )

    expect(described?.label).toBe('agent active')
    expect(described?.title).toContain('9s ago')
  })

  /**
   * The important one. After the window there is no evidence the agent is still
   * there, so the badge stops implying presence and says so outright.
   */
  test('past the window it drops the present tense and admits it cannot tell', () => {
    const described = describeAgent(
      activity({ calls: 4, inFlight: 0, lastAt: NOW - 60_000, lastTool: 'shareRead' }),
      NOW,
    )

    expect(described?.label).toBe('agent · 4 actions')
    expect(described?.tone).toBe('neutral')
    expect(described?.title).toMatch(/no longer be attached/)
  })

  test('an in-flight call outranks a stale timestamp', () => {
    const described = describeAgent(
      activity({ calls: 9, inFlight: 2, lastAt: NOW - 120_000, lastTool: 'shareSync' }),
      NOW,
    )

    expect(described?.label).toBe('agent controlling')
  })

  test('one action is not pluralised', () => {
    const described = describeAgent(
      activity({ calls: 1, inFlight: 0, lastAt: NOW - 60_000, lastTool: 'shareRead' }),
      NOW,
    )

    expect(described?.label).toBe('agent · 1 action')
  })
})
