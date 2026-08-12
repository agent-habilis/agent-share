import { Badge } from 'moonspace-dom'
import { component, disposable, interval, signal } from 'visage-dom'

import {
  agentActivity,
  subscribeAgentActivity,
  type AgentActivity,
} from '../../lib/agentTools/index.ts'

/**
 * Says when an agent is driving this page.
 *
 * The wording is careful, because the thing a person would want to know is not
 * observable. WebMCP lets a page publish tools; it never tells the page that
 * something has connected to them, and there is no agent-session concept in the
 * spec at all. A tab whose tools nobody has ever called looks exactly like a
 * tab no agent has found.
 *
 * So the badge is built out of the only real evidence — a tool being invoked —
 * and it never claims more than that. It is absent until the first call, says
 * *controlling* while a call is actually running, *active* for a short window
 * after one, and then falls back to a past-tense line with the count. Nothing
 * here asserts that an agent is still attached, because nothing can.
 */

/** After the last call, how long the page still counts as under active use. */
const ACTIVE_MS = 15_000

/** Below this, "just now" reads better than a number. */
const JUST_NOW_MS = 5_000

export function describeAgent(
  activity: AgentActivity,
  now: number,
): { tone: 'accent' | 'info' | 'neutral'; label: string; title: string } | null {
  if (activity.calls === 0) return null

  const since = now - activity.lastAt
  const tool = activity.lastTool ?? 'a tool'
  const plural = activity.calls === 1 ? '1 action' : `${activity.calls} actions`

  if (activity.inFlight > 0) {
    return {
      tone: 'accent',
      label: 'agent controlling',
      title: `An agent is running ${tool} right now — ${plural} so far.`,
    }
  }
  if (since < ACTIVE_MS) {
    return {
      tone: 'info',
      label: 'agent active',
      title: `An agent used ${tool} ${since < JUST_NOW_MS ? 'just now' : `${Math.round(since / 1000)}s ago`} — ${plural} so far.`,
    }
  }
  return {
    tone: 'neutral',
    label: `agent · ${plural}`,
    title: `An agent last used ${tool} ${Math.round(since / 1000)}s ago. It may no longer be attached — the browser does not report that.`,
  }
}

export const AgentBadge = component(function* () {
  const activity = signal<AgentActivity>(agentActivity())
  // Time is the other input: "active" decays without anything being called, so
  // the badge has to re-read the clock as well as the store.
  const now = signal(Date.now())

  using _updates = disposable(subscribeAgentActivity((next) => (activity.value = next)))
  using _clock = interval(1000, () => (now.value = Date.now()))

  yield () => {
    const described = describeAgent(activity.value, now.value)
    if (!described) return null
    return (
      <Badge
        tone={described.tone}
        variant={described.tone === 'accent' ? 'solid' : 'outline'}
        title={described.title}
        // A stable hook for tests. The rendered text is not one: the style
        // system inlines a `<style>` inside the badge, so `textContent` opens
        // with a stylesheet rather than the label.
        data-testid="agent-badge"
        data-agent-state={described.tone === 'accent' ? 'controlling' : described.tone === 'info' ? 'active' : 'idle'}
      >
        {described.label}
      </Badge>
    )
  }
})
