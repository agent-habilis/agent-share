/**
 * A minute of throughput as one row of text.
 *
 * Text, not a canvas or an SVG: the row is exactly one cell tall whatever the
 * font does, it survives a copy out of the pane like every other number here,
 * and it needs no measuring pass. The block ramp is the same Unicode block the
 * design system's bar glyphs already come from, so it is single-width
 * everywhere the rest of the UI is.
 */

/**
 * The ramp, lowest first. Index 0 is a space rather than `▁`, and that is the
 * point of it: a tick where nothing moved has to be distinguishable from the
 * slowest tick that did. A baseline drawn at zero would claim traffic that
 * never happened.
 */
const RAMP = ' ▁▂▃▄▅▆▇█'

/**
 * The last `cells` values, drawn against their own peak.
 *
 * Self-scaling rather than absolute, because the interesting question is the
 * *shape* — did it stall, is it ramping, is it sawtoothing — and an absolute
 * scale would flatten every transfer that is not near the fastest one ever
 * seen. The caller labels the peak, so the axis is never left implicit.
 *
 * Short history is left-padded, so the line grows in from the right and the
 * newest sample is always in the same place.
 */
export function sparkline(values: readonly number[], cells: number): string {
  if (cells <= 0) return ''
  const window = values.slice(-cells)
  const peak = Math.max(0, ...window)
  const drawn = window
    .map((value) => {
      if (!(value > 0) || peak <= 0) return RAMP[0]
      // `ceil` rather than `round`: any movement at all earns the first step of
      // the ramp, so a trickle reads as a trickle and not as silence.
      const step = Math.ceil((value / peak) * (RAMP.length - 1))
      return RAMP[Math.min(RAMP.length - 1, step)]
    })
    .join('')
  return drawn.padStart(cells, ' ')
}
