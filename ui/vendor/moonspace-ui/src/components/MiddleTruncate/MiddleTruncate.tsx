import { component, signal } from 'visage-dom'
import { msCss as css, msStyle } from '../../styles/css.ts'
import { middleTruncate } from '../../hooks/middleTruncate.ts'

export interface MiddleTruncateProps {
  value: string
  budget?: number
  class?: string
  id?: string
}

const WRAPPER = css({
  display: 'block',
  minWidth: 0,
  overflow: 'hidden',
  whiteSpace: 'nowrap',
})

function measureBudget(el: HTMLElement): number {
  const probe = document.createElement('span')
  probe.style.cssText =
    'position:absolute;visibility:hidden;white-space:pre;font:inherit;letter-spacing:inherit;'
  probe.textContent = '0'.repeat(100)
  el.appendChild(probe)
  const cell = probe.getBoundingClientRect().width / 100
  probe.remove()
  if (cell <= 0) return 0
  return Math.max(0, Math.floor(el.getBoundingClientRect().width / cell))
}

export const MiddleTruncate = component<MiddleTruncateProps>(function* (props) {
  const budget = signal<number | null>(null)

  yield () => {
    const text =
      props.budget !== undefined
        ? middleTruncate(props.value, props.budget)
        : budget.value === null
          ? props.value
          : middleTruncate(props.value, budget.value)

    return (
      <span
        class={props.class}
        id={props.id}
        title={props.value}
        ref={
          props.budget === undefined
            ? (el) => {
                const node = el as HTMLElement
                const update = () => {
                  budget.value = measureBudget(node)
                }
                update()
                const ro = new ResizeObserver(update)
                ro.observe(node)
                if ('fonts' in document) {
                  void document.fonts.ready.then(update)
                }
                return () => ro.disconnect()
              }
            : undefined
        }
      >
        {msStyle(WRAPPER)}
        {text}
      </span>
    )
  }
})
