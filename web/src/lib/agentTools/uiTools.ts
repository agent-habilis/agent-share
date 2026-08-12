/**
 * Tools that move the interface, not the share.
 *
 * These are the ones that match what WebMCP is actually for: a person has the
 * page open and an agent helps inside it. So they change what is on screen —
 * the selected folder, the current view — and they trigger the same actions the
 * buttons do.
 *
 * Two of them cannot be made to work unattended, and say so rather than
 * pretending. `shareDownload` needs `showSaveFilePicker` and `shareMount` needs
 * `showDirectoryPicker`; both require a real user gesture, and no directory
 * handle is persisted anywhere, so there is nothing to reuse from last time.
 * Driven headlessly they return `needs_user_gesture`. With a person at the
 * keyboard they work, which is the case worth serving.
 *
 * They all require a share page to be mounted. On `/` there is no session, and
 * the honest answer is to say so rather than to open one invisibly.
 */

import { locate } from './entries.ts'
import {
  fail,
  guard,
  ok,
  optionalString,
  requiredString,
  sharePathParts,
  ToolInputError,
} from './result.ts'
import { peekTree } from './session.ts'
import { agentSession, type AgentSession, type ShareViewName } from './uiBridge.ts'

const VIEWS: readonly ShareViewName[] = ['files', 'info', 'preview']

/** How long to let a fire-and-forget UI action declare failure before reporting. */
const SETTLE_MS = 700

function object(properties: object, required: string[] = []): object {
  return { type: 'object', properties, required, additionalProperties: false }
}

/**
 * The mounted session, or the failure every interface tool shares.
 *
 * Thrown rather than returned so each tool reads as one straight line; `guard`
 * converts it, and a `ToolInputError` keeps the code it chose.
 */
function requireUi(): AgentSession {
  const session = agentSession()
  if (!session) {
    throw new ToolInputError(
      'no_session',
      'No share page is open. Navigate to /files/<ticket> before using the interface tools.',
    )
  }
  return session
}

/**
 * Check a path against the share, but only if the tree is already loaded.
 *
 * Navigating somewhere that does not exist would leave the browser showing an
 * empty column with no explanation, so it is worth catching. Not worth a
 * connection, though: if nothing is cached the navigation simply goes ahead.
 */
function validate(ticket: string, parts: string[]): void {
  if (parts.length === 0) return
  const root = peekTree(ticket)
  if (root) locate(root, parts)
}

function snapshot(session: AgentSession) {
  return {
    ticket: session.ticket,
    view: session.view(),
    selection: session.selection(),
    status: session.status(),
    mounted: session.mounted(),
    transfer: session.transfer(),
  }
}

const shareUiState: ModelContextTool = {
  name: 'shareUiState',
  description:
    'Report what the person using this page is looking at: the current view, ' +
    'the selected file or folder, transfer status, and whether the share is ' +
    'mounted to a local directory.',
  inputSchema: object({}),
  annotations: { readOnlyHint: true, untrustedContentHint: false },
  execute: () =>
    guard(async () => {
      const session = agentSession()
      if (!session) {
        return fail('no_session', 'No share page is open. Navigate to /files/<ticket> first.')
      }
      return ok(snapshot(session))
    }),
}

const shareNavigate: ModelContextTool = {
  name: 'shareNavigate',
  description:
    'Move the file browser to a folder or file, changing what the person on ' +
    'this page sees. Use an empty path to go back to the top of the share.',
  inputSchema: object(
    {
      path: {
        type: 'string',
        description: 'Path to select, like "src/mount". Empty selects the share root.',
      },
    },
    [],
  ),
  annotations: { readOnlyHint: false, untrustedContentHint: false },
  execute: (input) =>
    guard(async () => {
      const session = requireUi()
      const parts = sharePathParts(optionalString(input, 'path'))
      validate(session.ticket, parts)
      session.select(parts)
      // The snapshot is read *after* the move, so its `selection` is the new
      // one — spreading `parts` over it would say the same thing twice.
      return ok(snapshot(session))
    }),
}

const shareOpenView: ModelContextTool = {
  name: 'shareOpenView',
  description:
    'Switch this page between the file browser, the connection info panel, and ' +
    'the file preview. Preview needs a path, or uses the current selection.',
  inputSchema: object(
    {
      view: {
        type: 'string',
        enum: ['files', 'info', 'preview'],
        description: 'Which view to show.',
      },
      path: { type: 'string', description: 'File to preview. Only used by the preview view.' },
    },
    ['view'],
  ),
  annotations: { readOnlyHint: false, untrustedContentHint: false },
  execute: (input) =>
    guard(async () => {
      const session = requireUi()
      const view = requiredString(input, 'view') as ShareViewName
      if (!VIEWS.includes(view)) {
        return fail('bad_argument', '"view" must be one of files, info, preview')
      }
      const raw = optionalString(input, 'path')
      const parts = raw === undefined ? session.selection() : sharePathParts(raw)
      if (view === 'preview' && parts.length === 0) {
        return fail('bad_argument', 'Previewing needs a file — pass "path".')
      }
      validate(session.ticket, parts)
      // The path only means anything to `preview`; the other two views are
      // whole-share routes that reject a trailing segment.
      const carried = view === 'preview' ? parts : []
      session.openView(view, carried)
      return ok({ view, path: carried })
    }),
}

/**
 * Run a fire-and-forget UI action and report what happened to it.
 *
 * The actions do not return anything — they set signals the bar renders — so
 * this triggers one, gives it a moment to fail, and reads the result back. A
 * refused picker lands in `errors()` well inside that window; a transfer that
 * started is visible in `transfer()`.
 */
async function settle(
  session: AgentSession,
  which: 'download' | 'mount' | 'seed',
  start: () => void,
): Promise<ReturnType<typeof ok> | ReturnType<typeof fail>> {
  const before = session.errors()[which]
  start()
  await new Promise((resolve) => setTimeout(resolve, SETTLE_MS))
  const error = session.errors()[which]

  if (error && error !== before) {
    const gesture = /gesture|user activation|NotAllowedError|SecurityError/i.test(error)
    return fail(
      gesture ? 'needs_user_gesture' : 'failed',
      gesture
        ? `The browser refused: ${error}. This action opens a file dialog, which only ` +
          'a real click can do — ask the person at this page to press the button.'
        : error,
    )
  }
  return ok({ started: true, ...snapshot(session) })
}

const shareDownload: ModelContextTool = {
  name: 'shareDownload',
  description:
    'Download the selected file or folder to the person\'s computer, as the ' +
    'Download button does. Opens a save dialog, so it only works when someone ' +
    'is at the page — it cannot run unattended.',
  inputSchema: object({
    path: { type: 'string', description: 'What to download. Defaults to the current selection.' },
  }),
  annotations: { readOnlyHint: false, untrustedContentHint: false },
  execute: (input) =>
    guard(async () => {
      const session = requireUi()
      const raw = optionalString(input, 'path')
      if (raw !== undefined) {
        const parts = sharePathParts(raw)
        validate(session.ticket, parts)
        session.select(parts)
      }
      return settle(session, 'download', () => session.download())
    }),
}

const shareMount: ModelContextTool = {
  name: 'shareMount',
  description:
    'Mirror the share into a folder on the person\'s computer, as the Mount ' +
    'button does, or drop an existing mirror. Opens a folder dialog, so it ' +
    'only works when someone is at the page.',
  inputSchema: object({}),
  annotations: { readOnlyHint: false, untrustedContentHint: false },
  execute: () =>
    guard(async () => {
      const session = requireUi()
      return settle(session, 'mount', () => session.mount())
    }),
}

const shareSeedSelection: ModelContextTool = {
  name: 'shareSeedSelection',
  description:
    'Take the currently selected folder into this browser and start serving it ' +
    'to other peers, as the Seed button does. Needs no dialog.',
  inputSchema: object({
    path: { type: 'string', description: 'What to seed. Defaults to the current selection.' },
  }),
  annotations: { readOnlyHint: false, untrustedContentHint: false },
  execute: (input) =>
    guard(async () => {
      const session = requireUi()
      const raw = optionalString(input, 'path')
      if (raw !== undefined) {
        const parts = sharePathParts(raw)
        validate(session.ticket, parts)
        session.select(parts)
      }
      return settle(session, 'seed', () => session.seed())
    }),
}

export const UI_TOOLS: readonly ModelContextTool[] = [
  shareUiState,
  shareNavigate,
  shareOpenView,
  shareDownload,
  shareMount,
  shareSeedSelection,
]
