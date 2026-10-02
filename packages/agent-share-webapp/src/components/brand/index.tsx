import { Text } from 'moonspace-dom'
import { component, disposable, interval, signal } from 'visage-dom'
import { Style, css, raw } from 'visage-style'

import {
  agentActivity,
  subscribeAgentActivity,
  type AgentActivity,
} from '../../lib/webmcp/index.ts'

/**
 * The app's name, which is also where it says an agent is using the page.
 *
 * The wording of that claim is the careful part, and it is the reason this is
 * colour rather than a label. WebMCP lets a page publish tools; it never tells
 * the page that something has connected to them, and the spec has no notion of
 * an agent session at all. A tab whose tools nobody has called looks exactly
 * like a tab no agent has found.
 *
 * So there are only two things worth showing, and both are past tense: a tool
 * has been called at some point (green, and it stays green — the page cannot
 * un-know it), and one was called just now (the letters shimmer). Neither
 * claims an agent is still attached, because nothing can. Do not grow this into
 * a connected light; there is nothing to drive one with.
 */

/** After the last call, how long the page still counts as under active use. */
const ACTIVE_MS = 10_000

/** Below this, "just now" reads better than a number. */
const JUST_NOW_MS = 5_000

const BRAND = 'agent-share'

export interface BrandState {
  /** A tool has been called at some point in this page's life. */
  green: boolean
  /** One is running, or ran within [`ACTIVE_MS`]. */
  animated: boolean
  /**
   * The whole state in words, for the native tooltip.
   *
   * Always present, including before anything has happened. Colour says *that*
   * something is going on and never what — which tool, how many times, how long
   * ago — so hovering the name is where all of it lives. The quiet state earns
   * a sentence too: a name that is not green because no agent has called
   * anything looks exactly like one that is not green because this browser has
   * no WebMCP to call.
   */
  title: string
}

export function describeBrand(activity: AgentActivity, now: number): BrandState {
  if (activity.calls === 0) {
    return {
      green: false,
      animated: false,
      title:
        activity.registered.length === 0
          ? 'This browser has no WebMCP, so this page publishes no tools an agent could call.'
          : `${activity.registered.length} tools published for an agent to use. None has been called yet.`,
    }
  }

  const since = now - activity.lastAt
  // Quoted, because the names are bare verbs: "An agent used sync" reads as a
  // sentence about syncing, "An agent used "sync"" names the tool that ran.
  const tool = activity.lastTool ? `"${activity.lastTool}"` : 'a tool'
  const plural = activity.calls === 1 ? '1 action' : `${activity.calls} actions`

  if (activity.inFlight > 0) {
    return {
      green: true,
      // Not on the clock. A `sync` pulling a large share runs for a long
      // time, and a rule that only counted `lastAt` would stop moving halfway
      // through the one call an agent is most obviously in the middle of.
      animated: true,
      title: `An agent is running ${tool} right now — ${plural} so far.`,
    }
  }
  if (since < ACTIVE_MS) {
    return {
      green: true,
      animated: true,
      title: `An agent used ${tool} ${
        since < JUST_NOW_MS ? 'just now' : `${Math.round(since / 1000)}s ago`
      } — ${plural} so far.`,
    }
  }
  return {
    green: true,
    animated: false,
    title: `An agent last used ${tool} ${Math.round(since / 1000)}s ago — ${plural} in total. It may no longer be attached; the browser does not report that.`,
  }
}

/** How far apart in the cycle two neighbouring characters sit. */
const STEP_MS = 80

/**
 * The bright spot, travelling one character at a time.
 *
 * Every letter runs the same keyframe; the offset between them is the whole
 * effect. Each character starts `--i × 80ms` later than the one before, so at
 * any instant they are at eleven different points of the cycle and the peak
 * moves along the word left to right. Ten steps of 80ms spread across a 1.1s
 * cycle keeps one peak in flight at a time rather than several.
 *
 * The delay is read from `--i`, set per character as an inline value. A `css()`
 * object per letter would compile the same rule eleven times, which
 * `visage-style` warns about in development, and is why the design system
 * passes per-element values through custom properties too.
 */
const SHIMMER = css({
  /*
    Two things about this selector, both learned the hard way.

    `span`, not bare declarations: `Style()` scopes to its *parent*, so at the
    top level this animates the whole word as one element — where `--i` does not
    exist, so every character shares one phase and the name blinks.

    Gated on the attribute rather than by rendering this `<style>` only when
    animating, so the element is the same one in both states and only the
    attribute changes. Swapping a conditional stylesheet in and out of the
    children left the animation applied after it should have stopped.
  */
  '&[data-agent="working"] span': {
    animation: raw('agent-shimmer 1.1s linear infinite'),
    animationDelay: raw(`calc(var(--i) * ${STEP_MS}ms)`),
    // Each character waits its turn on the very first pass, and without this it
    // waits at the *inherited* full green — so for the first second the right
    // half of the name sits bright while the left half is already moving.
    // `backwards` holds the 0% frame through the delay instead.
    animationFillMode: raw('backwards'),
    // The word stays green; only the movement stops. `@media` inside `css()`
    // works — it is `@keyframes` that cannot live there.
    '@media (prefers-reduced-motion: reduce)': { animation: raw('none') },
  },
})

export const Brand = component(function* () {
  const activity = signal<AgentActivity>(agentActivity())
  // The other input. "Active" decays with time alone, so without a clock
  // nothing would repaint when the ten seconds run out.
  const now = signal(Date.now())

  using _updates = disposable(subscribeAgentActivity((next) => (activity.value = next)))
  using _clock = interval(1000, () => (now.value = Date.now()))

  yield () => {
    const state = describeBrand(activity.value, now.value)

    // Untouched until an agent has actually called something: one element, no
    // spans, exactly the markup that was here before any of this existed.
    if (!state.green) {
      return (
        <Text weight="bold" title={state.title} data-agent="idle">
          {BRAND}
        </Text>
      )
    }

    return (
      <Text
        weight="bold"
        /*
          The component's own colour prop rather than a `color` declaration of
          ours. `Text` drives itself from `--ms-text-color` in the `moonspace`
          layer, and a rule of ours setting `color` on the same element loses to
          it — the name stayed white while only the animated characters showed
          any green at all.
        */
        color="success"
        title={state.title}
        data-agent={state.animated ? 'working' : 'green'}
        /*
          The letters are decoration once they are split. Without this a screen
          reader spells the name out one character at a time.
        */
        aria-label={BRAND}
      >
        {Style(SHIMMER)}
        {[...BRAND].map((char, index) => (
          <span key={index} aria-hidden="true" style={{ '--i': String(index) }}>
            {char}
          </span>
        ))}
      </Text>
    )
  }
})
