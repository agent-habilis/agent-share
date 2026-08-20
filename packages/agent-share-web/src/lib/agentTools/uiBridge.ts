/**
 * The seam that lets a tool drive the interface rather than the share.
 *
 * The tools under `lib/` know nothing about components, and should not: they
 * run on every route, including `/`, where no session is mounted. But "control
 * the application" means moving what a person is actually looking at, and that
 * state lives inside the `Session` component.
 *
 * So `Session` publishes a small adapter here while it is mounted, and the UI
 * tools ask for it. The interface is declared in this file — structurally,
 * naming only what a tool needs — so nothing under `lib/` has to import from
 * `components/`. `SessionApi` is the richer thing; this is the part an agent is
 * allowed to touch.
 *
 * At most one session is mounted at a time, so this holds one.
 */

export type ShareViewName = 'files' | 'info' | 'preview'

export interface TransferState {
  kind: string
  /** Bytes moved so far, and the total when it is known. */
  done: number
  total: number
}

export interface AgentSession {
  readonly ticket: string
  /** The column browser's current selection, one segment per level. */
  selection(): string[]
  select(path: string[]): void
  /** Which view is rendered, and move to another. */
  view(): ShareViewName
  openView(view: ShareViewName, file?: string[]): void
  status(): string
  mounted(): boolean
  transfer(): TransferState | null
  /**
   * Whatever the last download, mount or seed attempt reported.
   *
   * The actions are fire-and-forget — the UI shows a failure in the bar rather
   * than throwing — so a tool that triggered one has nowhere else to look for
   * the outcome. This is how a refused folder picker reaches the agent.
   */
  errors(): { download: string | null; mount: string | null; seed: string | null }
  /** Start the download of the current selection. May need a user gesture. */
  download(): void
  /** Mirror into a host directory, or drop the mirror. Always needs a gesture. */
  mount(): void
  /** Take the current selection into local storage and seed it. */
  seed(): void
}

let current: AgentSession | null = null

export function publishAgentSession(session: AgentSession): () => void {
  current = session
  return () => {
    if (current === session) current = null
  }
}

export function agentSession(): AgentSession | null {
  return current
}
