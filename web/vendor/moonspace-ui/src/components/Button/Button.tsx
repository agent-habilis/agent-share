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
 * The affordance is the box: a fill for the two variants that carry weight, an
 * outline for the ones that don't. It used to be a pair of `[ ]` brackets in
 * pseudo-elements, which meant `primary` painted its accent edge-to-edge across
 * them with no breathing room and read as a highlighted string rather than a
 * control. `oneRowChrome` supplies the padding, the border and the box; only
 * colour varies below.
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
    outlineColor: T.msBorderStrong,
  },
  '&[data-variant="ghost"]': {
    color: T.msFgMuted,
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
   * Hover fills the box with the colour already outlining it, so the border
   * reads as the button growing into itself rather than as a second, unrelated
   * cue. Replaces the underline the bracket-era button used: with a box to
   * fill, underlining the label only added a third thing moving at once.
   */
  '&[data-variant="secondary"]:hover:not(:disabled)': {
    background: T.msBorderStrong,
  },
  '&[data-variant="ghost"]:hover:not(:disabled)': {
    background: T.msBorderStrong,
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
