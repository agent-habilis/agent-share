/**
 * Moving between share routes.
 *
 * Thin over `visage-router`: the router owns matching and history, this owns
 * the two things it has no opinion about — carrying the local `?transport=`
 * and `?dev=` preferences across a navigation, and telling an entry this tab
 * pushed from one it was handed.
 */

import type { Ctx } from 'visage-dom'
import { useLocation, useNavigate } from 'visage-router'

import { parseDev, parseTransport, sharePath, type ShareView } from '../lib/ticket/index.ts'

/**
 * Stamped on every entry this tab pushes.
 *
 * The initial entry of a document — a cold load, a pasted link, a new tab — was
 * not pushed by anyone here, and nothing can be behind it. That difference is
 * what lets a view decide between popping back to where the user came from and
 * replacing itself: `back()` off a pasted link leaves the site.
 *
 * It rides the router's *user* state, which lands under `history.state.usr` —
 * the top level belongs to the router's own entry key.
 */
const PUSHED = { agentShare: 1 }

/** The user half of the current history entry, whatever the router put around it. */
function userState(): unknown {
  return (window.history.state as { usr?: unknown } | null | undefined)?.usr
}

/** Whether the current entry was pushed by this tab, so `back()` stays here. */
export function canGoBack(state: unknown = userState()): boolean {
  return typeof state === 'object' && state !== null && 'agentShare' in state
}

/**
 * Where a share view lives, with the current query preferences carried forward.
 *
 * Without the carry, switching `/files` ↔ `/info` would drop the `?transport=`
 * pin and silently redial the share in a different mode — so the Info pane you
 * opened to inspect a WebRTC session would be reporting on a fresh dynamic one.
 * `dev` rides along for the plainer reason that the dev tools live *in* that
 * pane, so dropping the flag on the way to it would make them unreachable.
 */
export function shareTarget(
  search: string,
  ticket: string,
  view: ShareView = 'files',
  file?: readonly string[],
): string {
  return sharePath(ticket, view, parseTransport(search), parseDev(search), file)
}

export interface ShareNav {
  /** Go to a share view. Pushes unless `replace` says otherwise. */
  go(
    ticket: string,
    view?: ShareView,
    options?: { replace?: boolean; file?: readonly string[] },
  ): void
  /** Step back one history entry. Only sound when [`canGoBack`] holds. */
  back(): void
}

export function useShareNav(ctx: Ctx): ShareNav {
  const navigate = useNavigate(ctx)
  const location = useLocation(ctx)
  return {
    go(ticket, view = 'files', options) {
      const target = shareTarget(location.peek().search, ticket, view, options?.file)
      if (options?.replace) {
        // The entry keeps its place in history, so it keeps its stamp —
        // replacing a pasted link must not make it look like something can be
        // behind it. The router writes no `usr` at all when `state` is omitted,
        // which would quietly clear it.
        navigate(target, { replace: true, state: userState() })
      } else {
        navigate(target, { state: PUSHED })
      }
    },
    back() {
      navigate(-1)
    },
  }
}
