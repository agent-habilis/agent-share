/**
 * Whether an agent is driving this page, how recently, and what it called.
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
 * Deliberately free of the render layer, like the rest of `lib/`. The brand and
 * the `/info` panel subscribe and keep their own signals.
 */

export interface AgentCall {
  /** Monotonic, and the only safe way to find an entry again — see [`beginToolCall`]. */
  readonly id: number
  readonly tool: string
  /** One line, already redacted. Empty when the tool took no arguments. */
  readonly args: string
  readonly startedAt: number
  /** Null while the call is still running. */
  readonly endedAt: number | null
  /** `running`, then `ok` or the failure's `ToolErrorCode`. */
  readonly outcome: string
  readonly error: string | null
}

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
  /** The last [`LOG_LIMIT`] calls, newest first. */
  readonly log: readonly AgentCall[]
}

/**
 * How many calls are kept.
 *
 * A cap rather than the whole history, because nothing ever clears this and a
 * tab left open under an agent would grow it without bound. A hundred lines is
 * far more than the panel shows and still nothing next to a manifest.
 */
export const LOG_LIMIT = 100

const EMPTY: AgentActivity = {
  registered: [],
  calls: 0,
  inFlight: 0,
  lastAt: 0,
  lastTool: null,
  log: [],
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

let nextId = 1

/**
 * Record the start of a call, and return the function that ends it.
 *
 * Counted at the start rather than on completion: a `sync` pulling a large
 * share can run for a long time, and an indicator that only lit up afterwards
 * would be dark for exactly the period an agent was most obviously in control. The
 * log entry appears at the same moment, saying `running`.
 *
 * The end function finds its entry by `id` rather than by position. Calls
 * overlap and finish in any order, and the list is trimmed from the far end, so
 * an index taken at the start means something else by the time it is used.
 */
export function beginToolCall(name: string, input?: unknown): (result?: unknown) => void {
  const id = nextId++
  const startedAt = Date.now()
  const call: AgentCall = {
    id,
    tool: name,
    args: summarizeArgs(input),
    startedAt,
    endedAt: null,
    outcome: 'running',
    error: null,
  }
  emit({
    ...state,
    calls: state.calls + 1,
    inFlight: state.inFlight + 1,
    lastAt: startedAt,
    lastTool: name,
    log: [call, ...state.log].slice(0, LOG_LIMIT),
  })
  let ended = false
  return (result?: unknown) => {
    if (ended) return
    ended = true
    const endedAt = Date.now()
    emit({
      ...state,
      inFlight: Math.max(state.inFlight - 1, 0),
      lastAt: endedAt,
      log: state.log.map((entry) =>
        entry.id === id ? { ...entry, endedAt, ...readOutcome(result) } : entry,
      ),
    })
  }
}

/** How long an error may be before the log keeps only its opening. */
const ERROR_CHARS = 300

/**
 * What a tool returned, as a word and a sentence.
 *
 * `undefined` means the tool threw past its own `guard` — which should not
 * happen, and is worth showing as a failure rather than swallowing.
 *
 * The message is clipped because its length is the caller's: several failures
 * quote the path or ticket they were given back at the agent, and up to
 * [`LOG_LIMIT`] of them are held at once.
 */
function readOutcome(result: unknown): { outcome: string; error: string | null } {
  if (result === undefined || result === null) {
    return { outcome: 'failed', error: 'the tool threw' }
  }
  const shape = result as { ok?: unknown; code?: unknown; error?: unknown }
  if (shape.ok === true) return { outcome: 'ok', error: null }
  return {
    outcome: typeof shape.code === 'string' ? shape.code : 'failed',
    error: typeof shape.error === 'string' ? clip(shape.error, ERROR_CHARS) : null,
  }
}

/** Redacted, because an agent opening a protected share passes the password. */
const SECRET = /password|secret|token/i

const VALUE_CHARS = 24
const LINE_CHARS = 80

/**
 * A call's arguments as one line.
 *
 * Done here, when the call is recorded, rather than in the panel that draws it.
 * That way the password an agent passed to `connect` never enters the
 * store at all, and no later reader of the log has to remember to hide it.
 *
 * Nothing here is allowed to scale with what the agent sent. The browser does
 * not check a call against `inputSchema` (see `result.ts`), so an argument can
 * be a million-element array — serializing one to keep 24 characters of it cost
 * milliseconds on the main thread, so a container is labelled by its size
 * rather than written out, and the loop stops once the line is long enough.
 */
export function summarizeArgs(input: unknown): string {
  if (typeof input !== 'object' || input === null || Array.isArray(input)) return ''
  let line = ''
  for (const [key, value] of Object.entries(input)) {
    if (value === undefined) continue
    if (line.length > LINE_CHARS) return `${line}…`
    line += `${line === '' ? '' : ' '}${key}=${SECRET.test(key) ? '•••' : describeValue(value)}`
  }
  return clip(line, LINE_CHARS)
}

/** One argument, small enough to sit in a table cell whatever it holds. */
function describeValue(value: unknown): string {
  if (typeof value === 'string') return clip(value, VALUE_CHARS)
  if (Array.isArray(value)) return `[${value.length} items]`
  if (typeof value === 'object' && value !== null) {
    return `{${Object.keys(value).length} keys}`
  }
  return clip(String(value), VALUE_CHARS)
}

function clip(text: string, limit: number): string {
  return text.length > limit ? `${text.slice(0, limit)}…` : text
}

/** Test seam. */
export function resetAgentActivity(): void {
  emit(EMPTY)
}
