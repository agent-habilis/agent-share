/**
 * Publish this page's tools to an agent, using WebMCP.
 *
 * WebMCP (`document.modelContext`) is a W3C Community Group draft. No browser
 * enables it by default, so this is a no-op almost everywhere and must stay
 * cheap enough not to care — one property read, then nothing.
 *
 * There is deliberately no polyfill. The way an agent reaches these tools today
 * is Chrome's own `chrome-devtools-mcp`, which launches the browser and can
 * therefore turn the native feature on itself. A polyfill would add a
 * dependency to serve a case that does not exist: a browser without WebMCP is
 * also a browser with nothing on the other end to call it.
 *
 * See `docs/webmcp.md` for the connection setup.
 */

import { beginToolCall, markToolsRegistered } from './activity.ts'
import { TOOLS as SHARE_TOOLS } from './tools.ts'
import { UI_TOOLS } from './uiTools.ts'

export {
  agentActivity,
  subscribeAgentActivity,
  type AgentActivity,
  type AgentCall,
} from './activity.ts'
export { publishAgentSession, type AgentSession } from './uiBridge.ts'
export { resetSessions } from './session.ts'

/**
 * Everything this page publishes: the share itself, then the interface over it.
 *
 * The split is worth keeping in mind when reading them. The share tools work on
 * any route and need no one present. The interface tools move what a person is
 * looking at, so they need a share page mounted — and two of them need that
 * person to click something.
 */
export const TOOLS: readonly ModelContextTool[] = [...SHARE_TOOLS, ...UI_TOOLS]

/**
 * Wrap a tool so its invocations are visible to the page.
 *
 * Nothing tells a page that an agent has connected — WebMCP has no such signal
 * — so being *called* is the only evidence there is, and the colour of the name
 * in the top bar is built entirely out of it. Wrapping here rather than in each
 * tool means a tool cannot be added and quietly left out of the count.
 *
 * It is also the one place that sees the name, the arguments and the result of
 * every call, which is what the log on `/info` is made of.
 */
function instrument(tool: ModelContextTool): ModelContextTool {
  return {
    ...tool,
    execute: async (input) => {
      const end = beginToolCall(tool.name, input)
      // Left undefined by a throw, which is how `end` tells a failure that came
      // back as a result from one that escaped the tool's own `guard`.
      let result: unknown
      try {
        result = await tool.execute(input)
        return result
      } finally {
        end(result)
      }
    },
  }
}

/**
 * Guards against a second registration in one document.
 *
 * `registerTool` rejects a duplicate name with `InvalidStateError`, and the dev
 * server's hot reload re-runs the entry module against a document that still
 * holds the previous registration. Aborting the old batch first makes a reload
 * replace the tools rather than fail on the first one.
 */
let current: AbortController | undefined

export interface RegisterResult {
  /** False when this browser has no WebMCP, which is the common case. */
  registered: boolean
  names: string[]
}

export async function registerAgentTools(): Promise<RegisterResult> {
  const modelContext = document.modelContext
  if (!modelContext) return { registered: false, names: [] }

  current?.abort()
  const controller = new AbortController()
  current = controller

  // Registered together rather than one after another. Each `registerTool` is a
  // round trip, and until the last one lands an agent calling `getTools()` sees
  // a partial set — it would not be told the list is still filling, so it would
  // simply conclude the missing tools do not exist. Observed: listing right
  // after load returned three of eight.
  const settled = await Promise.all(
    TOOLS.map(async (tool) => {
      try {
        await modelContext.registerTool(instrument(tool), { signal: controller.signal })
        return tool.name
      } catch (error) {
        // One bad tool must not cost the others. A name collision with something
        // else on the page is the likely cause, and it is worth saying out loud.
        console.warn(`[agent-share] could not publish the "${tool.name}" tool`, error)
        return undefined
      }
    }),
  )

  const names = settled.filter((name): name is string => name !== undefined)
  markToolsRegistered(names)
  return { registered: names.length > 0, names }
}

/** Withdraw the tools. Aborting the signal is the only way to unregister. */
export function unregisterAgentTools(): void {
  current?.abort()
  current = undefined
}
