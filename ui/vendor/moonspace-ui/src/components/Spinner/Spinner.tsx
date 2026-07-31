import { component, signal } from 'visage-dom'
import { interval } from 'visage-dom/resources'
import { msCss as css, msStyle } from '../../styles/css.ts'
import { oneRow, srOnly } from '../../styles/mixins.ts'
import { roleVar } from '../../tokens.ts'
import { asciiSpinnerFrames, spinnerFrames } from '../../theme/glyphs.ts'
import type { ColorRole } from '../../theme/semantic.ts'

export interface SpinnerProps {
  interval?: number
  ascii?: boolean
  color?: ColorRole
  label?: string
  class?: string
  id?: string
}

const WRAPPER = css({
  ...oneRow,
  display: 'inline-flex',
  alignItems: 'center',
  gap: '1ch',
})

const FRAME = css({
  flex: 'none',
  width: '1ch',
})

const HIDDEN = css(srOnly)

export const Spinner = component<SpinnerProps>(function* (props) {
  const frameIndex = signal(0)
  const ms = props.interval ?? 80
  const frames = props.ascii ? asciiSpinnerFrames : spinnerFrames
  const color = props.color ?? 'accent'
  const label = props.label ?? 'Loading'

  // `using` is block-scoped: keep the timer at generator scope so the `if`
  // cannot dispose it before the first yield.
  const reducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches
  using _tick = reducedMotion
    ? { [Symbol.dispose]() {} }
    : interval(ms, () => {
        frameIndex.value = (frameIndex.value + 1) % frames.length
      })

  yield () => (
    <span role="status" class={props.class} id={props.id}>
      {msStyle(WRAPPER)}
      <span aria-hidden="true" style={{ color: roleVar[color] }}>
        {msStyle(FRAME)}
        {frames[frameIndex.value % frames.length]}
      </span>
      <span>
        {msStyle(HIDDEN)}
        {label}
      </span>
    </span>
  )
})
