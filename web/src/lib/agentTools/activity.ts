/**
 * Whether an agent is driving this page, and how recently.
 *
 * There is no way to ask. WebMCP publishes tools; it does not tell a page that
 * something has connected to them, and the spec has no notion of an agent
 * session at all. `getTools()` is a *caller's* API — a page calling it learns
 * about its own tools, not about who else is looking.
 *
 * So the only honest evidence is a tool actually being invoked. That is what
 * this records, and it is why the UI says "an agent is using this page" rather
 * than "an agent is connected": the second would be a claim nothing can
 * support. A tab with tools published and nobody calling them is
 * indistinguishable from a tab no agent has ever found.
 *
 * Deliberately free of the render layer, like the rest of `lib/`. The badge
 * subscribes and keeps its own signal.
 */

export interface AgentActivity {
  /** Tool names successfully published. Empty when this browser has no WebMCP. */
  readonly registered: readonly string[]
  /** Tool invocations seen since load. */
  readonly calls: number
  /** Calls started and not yet finished — an agent acting *right now*. */
  readonly inFlight: number
  /** Epoch ms of the most recent invocation, or 0 if there has never been one. */
  readonly lastAt: number
  readonly lastTool: string | null
}

const EMPTY: AgentActivity = {
  registered: [],
  calls: 0,
  inFlight: 0,
  lastAt: 0,
  lastTool: null,
}

let state: AgentActivity = EMPTY
const listeners = new Set<(activity: AgentActivity) => void>()

function emit(next: AgentActivity): void {
  state = next
  for (const listener of listeners) listener(state)
}

export function agentActivity(): AgentActivity {
  return state
}

export function subscribeAgentActivity(listener: (activity: AgentActivity) => void): () => void {
  listeners.add(listener)
  return () => listeners.delete(listener)
}

export function markToolsRegistered(names: readonly string[]): void {
  emit({ ...state, registered: [...names] })
}

/**
 * Record the start of a call, and return the function that ends it.
 *
 * Counted at the start rather than on completion: a `shareSync` pulling a large
 * share can run for a long time, and a badge that only lit up afterwards would
 * be dark for exactly the period an agent was most obviously in control.
 */
export function beginToolCall(name: string): () => void {
  emit({
    ...state,
    calls: state.calls + 1,
    inFlight: state.inFlight + 1,
    lastAt: Date.now(),
    lastTool: name,
  })
  let ended = false
  return () => {
    if (ended) return
    ended = true
    emit({ ...state, inFlight: Math.max(state.inFlight - 1, 0), lastAt: Date.now() })
  }
}

/** Test seam. */
export function resetAgentActivity(): void {
  emit(EMPTY)
}
