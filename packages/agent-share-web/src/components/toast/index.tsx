import { Badge, Button, Text, t } from 'moonspace-dom'

import type { ToastMessage } from '../session/session.ts'

/*
  Hoisted, because a fresh object every render defeats the memo every child of
  this bar relies on: `Text` compares its read props with `Object.is`, and the
  page's thunk re-runs once per transfer chunk while a toast is up.
*/
const BADGE = { justifySelf: 'start' }
const MESSAGE = { textAlign: 'center', maxWidth: String(t.msMeasure) }
const CLOSE = { justifySelf: 'end' }

/**
 * What went wrong, said in the top bar rather than under it.
 *
 * The bar is exactly `oneRow` tall and everything below it is positioned by
 * that fact, so a message cannot have a row of its own — an earlier strip below
 * the bar moved every pixel of the page each time something failed. This takes
 * over the row instead, the same move a running transfer makes with the actions.
 *
 * The whole row, brand included — a failure is the only thing the bar is saying
 * while it is up, so nothing shares it. The badge stands where the brand does,
 * which is why it is the one part of this that is not centred.
 *
 * Three cells rather than a grid: they drop straight into `Chrome`'s own rails,
 * so the message lands on the same centre the transfer readout it replaced was
 * drawn on. The comment on that grid says why the centre is the container's and
 * not the slack between the two ends; a grid here would restate it and could
 * then drift from it.
 *
 * One line, always, and at most 80 columns of it: `truncate` clips a long
 * browser message rather than wrapping it into a second row, and the measure
 * stops it stretching the width of a wide window on the way. Clipped in the
 * layout rather than sliced in JS, so the whole text stays reachable through
 * the tooltip and stays copyable — these are diagnostics, destined for a bug
 * report. See `.selectable` in `app.css`.
 */
export function Toast({ tone, message, onClose }: ToastMessage & { onClose: () => void }) {
  // One name for both: `Badge` and `Text` take the same palette role.
  const role = tone === 'error' ? 'danger' : 'warning'

  return (
    <>
      {/*
        Where the brand was. It says which of the two tones this is in a word,
        for anyone who cannot tell red from yellow — the colour alone would be
        the only thing carrying that.
      */}
      <Badge tone={role} variant="solid" style={BADGE}>
        {tone}
      </Badge>
      <Text
        color={role}
        truncate
        title={message}
        class="selectable"
        // A stable hook for tests: the style system inlines a `<style>` inside
        // the element, so `textContent` opens with a stylesheet rather than the
        // message.
        data-testid="toast"
        style={MESSAGE}
      >
        {message}
      </Text>
      {/*
        A word, not a glyph: every other control in this bar is one, and a `×`
        would be the only thing here that has to be guessed at. No fill and no
        border, because the colour plus the takeover is the signal already and a
        padded box would put the row's height back in play — which is the one
        thing this component exists to hold still.
      */}
      <Button variant="ghost" onclick={onClose} style={CLOSE}>
        Close
      </Button>
    </>
  )
}
