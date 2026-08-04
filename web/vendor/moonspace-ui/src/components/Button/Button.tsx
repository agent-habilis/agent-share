import type { Child } from 'visage-dom'
import { raw } from 'visage-style'
import { msCss as css, msStyle } from '../../styles/css.ts'
import { disabled, oneRowChrome } from '../../styles/mixins.ts'
import { T } from '../../tokens.ts'

export type ButtonVariant = 'primary' | 'secondary' | 'ghost' | 'danger'

export interface ButtonProps {
  variant?: ButtonVariant
  block?: boolean
  type?: 'button' | 'submit' | 'reset'
  disabled?: boolean
  children?: Child
  class?: string
  id?: string
  onclick?: (event: MouseEvent) => void
  'aria-label'?: string
}

/*
 * The affordance is the box: a fill for the three variants that carry weight,
 * nothing at all for `ghost`. It used to be a pair of `[ ]` brackets in
 * pseudo-elements, which meant `primary` painted its accent edge-to-edge across
 * them with no breathing room and read as a highlighted string rather than a
 * control. `oneRowChrome` supplies the padding, the border and the box; only
 * colour varies below.
 *
 * `ghost` carries `secondary`'s text colour and none of its fill. Muted text
 * was the earlier distinction, and it made the button read as de-emphasised
 * prose rather than as a control of equal rank sitting one weight down — the
 * missing box already says that, and says it without touching legibility.
 */
const BUTTON = css({
  ...oneRowChrome,
  /*
   * Casing is the component's, not the caller's — the same call Badge, Note and
   * Table make. Doing it here rather than in the labels covers the one dynamic
   * label the app has, and leaves the accessible name capitalised, since
   * `text-transform` is presentation and screen readers read the DOM text.
   */
  textTransform: 'lowercase',
  '&[data-variant="primary"]': {
    background: T.msAccent,
    color: T.msFgOnAccent,
  },
  '&[data-variant="secondary"]': {
    color: T.msFg,
    background: T.msBgRaised,
  },
  '&[data-variant="ghost"]': {
    color: T.msFg,
  },
  '&[data-variant="danger"]': {
    background: T.msDanger,
    color: T.msFgOnAccent,
  },
  '&[data-block="true"]': {
    display: 'flex',
    justifyContent: 'center',
    '@supports (width: round(down, 100%, 1ch))': {
      width: raw('round(down, 100%, 1ch)'),
    },
  },
  /*
   * Hover is one step further up the same stack the button already sits on:
   * sunken page, raised button, this. Nothing is outlined, so the fill is the
   * only thing that can carry the state — which is also why the bracket-era
   * underline went, since it read as a second, unrelated cue.
   */
  '&[data-variant="secondary"]:hover:not(:disabled)': {
    background: T.msBorderStrong,
  },
  /*
   * Ghost steps up to exactly what `secondary` paints at rest, rather than to
   * `secondary`'s own hover. It is a weight below `secondary`, and hovering it
   * onto the same fill `secondary` hovers onto would collapse the two — and
   * leave ghost, the lighter control, looking heavier than the one above it.
   */
  '&[data-variant="ghost"]:hover:not(:disabled)': {
    background: T.msBgRaised,
  },
  /*
   * Focus recolours the edge rather than drawing a second ring, the way Input
   * does on `:focus-within` — there is only one outline to go around. The offset
   * stays inset with it; stepping the ring outside would put focus back in the
   * clipped band the edge was just moved out of. Inversion used to be the
   * signal, back when there was no box to outline.
   */
  '&:focus-visible': {
    outlineColor: T.msAccent,
  },
  /*
   * A ring has to contrast with what it surrounds, and `primary` is filled with
   * the ring's own colour — accent on accent is invisible. `danger` needs no
   * exception; a blue ring on red is already its own signal.
   */
  '&[data-variant="primary"]:focus-visible': {
    outlineColor: T.msFg,
  },
  '&:disabled': {
    ...disabled,
  },
  /*
   * Ghost is the one variant with no box to grey out, so it keeps none here
   * either — the subtle text `disabled` sets is the whole signal. Painting the
   * shared fill would make the box *appear* on the way to disabled and vanish
   * on the way back, which is the one thing the variant exists not to do.
   */
  '&:disabled:not([data-variant="ghost"])': {
    background: T.msBgRaised,
  },
})

export function Button({
  variant = 'secondary',
  block = false,
  type = 'button',
  children,
  class: className,
  ...rest
}: ButtonProps) {
  return (
    <button
      {...rest}
      {...(className !== undefined ? { class: className } : {})}
      type={type}
      data-variant={variant}
      data-block={block ? 'true' : 'false'}
    >
      {msStyle(BUTTON)}
      {children}
    </button>
  )
}
