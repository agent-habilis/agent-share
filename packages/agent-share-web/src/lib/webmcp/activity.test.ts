import { beforeEach, describe, expect, test } from 'bun:test'

import {
  agentActivity,
  beginToolCall,
  LOG_LIMIT,
  markToolsRegistered,
  resetAgentActivity,
  subscribeAgentActivity,
  summarizeArgs,
} from './activity.ts'

beforeEach(resetAgentActivity)

describe('what the page can know about an agent', () => {
  /**
   * The claim the UI must never make. WebMCP has no agent-session concept and
   * never tells a page that something connected, so publishing tools is not
   * evidence of anything — a tab nobody has found looks identical.
   */
  test('publishing tools on its own is not evidence of an agent', () => {
    markToolsRegistered(['read', 'list'])

    expect(agentActivity().registered).toEqual(['read', 'list'])
    expect(agentActivity().calls).toBe(0)
    expect(agentActivity().lastAt).toBe(0)
  })

  test('an invocation is', () => {
    beginToolCall('read')

    expect(agentActivity().calls).toBe(1)
    expect(agentActivity().lastTool).toBe('read')
    expect(agentActivity().lastAt).toBeGreaterThan(0)
  })
})

describe('in-flight tracking', () => {
  // Counted at the start, so a long sync shows as "controlling" while it
  // runs rather than only once it has finished.
  test('a call is in flight from the moment it starts', () => {
    beginToolCall('sync')

    expect(agentActivity().inFlight).toBe(1)
  })

  test('ending a call clears it', () => {
    const end = beginToolCall('sync')
    end()

    expect(agentActivity().inFlight).toBe(0)
    expect(agentActivity().calls).toBe(1)
  })

  test('overlapping calls are counted together', () => {
    const first = beginToolCall('read')
    beginToolCall('list')

    expect(agentActivity().inFlight).toBe(2)
    first()
    expect(agentActivity().inFlight).toBe(1)
  })

  // The wrapper calls `end` in a `finally`, and a retry or double-dispose must
  // not drive the counter below zero and leave the brand stuck shimmering.
  test('ending twice is harmless', () => {
    const end = beginToolCall('read')
    end()
    end()

    expect(agentActivity().inFlight).toBe(0)
  })

  test('finishing a call still counts as recent activity', () => {
    const end = beginToolCall('read')
    const started = agentActivity().lastAt
    end()

    expect(agentActivity().lastAt).toBeGreaterThanOrEqual(started)
  })
})

describe('the call log', () => {
  const log = () => agentActivity().log

  test('a call is in the log from the moment it starts, still running', () => {
    beginToolCall('read', { path: 'a.md' })

    expect(log()).toHaveLength(1)
    expect(log()[0]).toMatchObject({
      tool: 'read',
      args: 'path=a.md',
      outcome: 'running',
      endedAt: null,
    })
  })

  // Newest first, so the panel needs no auto-scroll: a new line lands where the
  // reader is already looking rather than below the fold.
  test('the newest call is first', () => {
    beginToolCall('list')
    beginToolCall('read')

    expect(log().map((call) => call.tool)).toEqual(['read', 'list'])
  })

  test('a success is recorded as ok, with a duration', () => {
    beginToolCall('list')({ ok: true, entries: [] })

    expect(log()[0]).toMatchObject({ outcome: 'ok', error: null })
    expect(log()[0]?.endedAt).toBeGreaterThanOrEqual(log()[0]!.startedAt)
  })

  // The code is what the panel shows; the prose is the hover. Both come off the
  // result, because a tool reports failure by returning rather than throwing.
  test('a failure keeps its code and its message', () => {
    beginToolCall('read')({ ok: false, code: 'not_found', error: 'no such path' })

    expect(log()[0]).toMatchObject({ outcome: 'not_found', error: 'no such path' })
  })

  // Nothing should reach this — every tool wraps itself in `guard` — so if it
  // does, the log has to say so rather than leave the line running forever.
  test('a throw past guard still closes the entry', () => {
    beginToolCall('sync')(undefined)

    expect(log()[0]?.outcome).toBe('failed')
    expect(log()[0]?.endedAt).not.toBeNull()
  })

  /**
   * The reason entries are found by id. Calls overlap, they finish in whatever
   * order their work allows, and the list is trimmed from the far end — so a
   * position taken when the call started names a different entry later.
   */
  test('overlapping calls finishing out of order each update their own entry', () => {
    const first = beginToolCall('sync')
    const second = beginToolCall('list')

    second({ ok: true })
    expect(log().map((call) => call.outcome)).toEqual(['ok', 'running'])

    first({ ok: false, code: 'no_session', error: 'nothing open' })
    expect(log().map((call) => call.outcome)).toEqual(['ok', 'no_session'])
  })

  test('ending twice does not reopen or duplicate the entry', () => {
    const end = beginToolCall('read')
    end({ ok: true })
    end(undefined)

    expect(log()).toHaveLength(1)
    expect(log()[0]?.outcome).toBe('ok')
  })

  // Nothing ever clears the log, so the cap is what keeps a tab left open under
  // an agent from growing it without bound.
  test('the log stops at the cap and drops the oldest', () => {
    for (let index = 0; index < LOG_LIMIT + 5; index += 1) {
      beginToolCall(`tool${index}`)()
    }

    expect(log()).toHaveLength(LOG_LIMIT)
    expect(log()[0]?.tool).toBe(`tool${LOG_LIMIT + 4}`)
    expect(log().at(-1)?.tool).toBe('tool5')
  })
})

describe('argument summaries', () => {
  /**
   * The whole reason summarizing happens when the call is recorded rather than
   * when it is drawn: `connect` and `publish` both take a password,
   * and this way the real one never enters the store for a later reader to
   * leak by accident.
   */
  test('a password is replaced, not shortened', () => {
    const line = summarizeArgs({ ticket: 'ag1qx', password: 'hunter2' })

    expect(line).toBe('ticket=ag1qx password=•••')
    expect(line).not.toContain('hunter2')
  })

  test('a long value is truncated', () => {
    const line = summarizeArgs({ ticket: 'a'.repeat(80) })

    expect(line.length).toBeLessThan(40)
    expect(line).toContain('…')
  })

  test('a tool called with nothing gets an empty line, not "{}"', () => {
    expect(summarizeArgs({})).toBe('')
    expect(summarizeArgs(undefined)).toBe('')
  })

  // The browser hands `execute` whatever the agent sent, so a missing optional
  // arrives as `undefined` — a column of `transport=undefined` says nothing.
  test('an argument the agent left out is not listed', () => {
    expect(summarizeArgs({ path: 'a.md', transport: undefined })).toBe('path=a.md')
  })

  test('a non-string value is readable', () => {
    expect(summarizeArgs({ maxBytes: 4096, recursive: true })).toBe(
      'maxBytes=4096 recursive=true',
    )
  })

  /**
   * The browser does not check a call against `inputSchema`, so the size of an
   * argument is the agent's choice, not the schema's. Serializing a container
   * to keep a few characters of it put that choice on the main thread.
   */
  test('a container is labelled by its size, not written out', () => {
    const huge = { paths: Array.from({ length: 100_000 }, (_, index) => `f${index}.txt`) }

    const line = summarizeArgs(huge)

    expect(line).toBe('paths=[100000 items]')
    expect(line).not.toContain('f0.txt')
  })

  test('an object argument is labelled the same way', () => {
    expect(summarizeArgs({ filter: { kind: 'file', held: true } })).toBe('filter={2 keys}')
  })

  // A line stops as soon as it is long enough, so a thousand keys cost no more
  // than the handful that fit.
  test('a line stops once it is long enough', () => {
    const many = Object.fromEntries(
      Array.from({ length: 1_000 }, (_, index) => [`key${index}`, 'value']),
    )

    const line = summarizeArgs(many)

    expect(line.endsWith('…')).toBe(true)
    expect(line.length).toBeLessThan(120)
  })
})

describe('what the log keeps of a failure', () => {
  // Several failures quote the argument they were handed back at the agent, so
  // the message length is the caller's — and up to LOG_LIMIT are held at once.
  test('a long error message is clipped', () => {
    beginToolCall('read')({ ok: false, code: 'not_found', error: 'x'.repeat(5_000) })

    const entry = agentActivity().log[0]
    expect(entry?.error?.length).toBeLessThan(400)
    expect(entry?.error?.endsWith('…')).toBe(true)
  })
})

describe('subscribers', () => {
  test('are told on every change', () => {
    const seen: number[] = []
    const stop = subscribeAgentActivity((activity) => seen.push(activity.calls))

    beginToolCall('read')()
    expect(seen.length).toBeGreaterThanOrEqual(2)
    stop()
  })

  test('stop hearing once unsubscribed', () => {
    let count = 0
    const stop = subscribeAgentActivity(() => (count += 1))
    stop()
    beginToolCall('read')

    expect(count).toBe(0)
  })

  test('the snapshot is replaced, not mutated, so a signal sees a new value', () => {
    const before = agentActivity()
    beginToolCall('read')

    expect(agentActivity()).not.toBe(before)
    expect(before.calls).toBe(0)
  })
})
