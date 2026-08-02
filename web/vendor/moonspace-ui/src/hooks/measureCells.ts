/**
 * How many grid cells fit across an element, measured in the live font.
 *
 * Everything in the system is drawn from characters, so a component that wants
 * to fill its box has to know that box in cells, not pixels. There is no way to
 * ask CSS — `1ch` depends on the resolved font, which is only knowable once the
 * element is in the document. So: append a hidden run of digits, divide, remove.
 *
 * Callers pair this with a `ResizeObserver` and `document.fonts.ready`; the
 * first measurement can land before the font does.
 */
export function measureCells(el: HTMLElement): number {
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
