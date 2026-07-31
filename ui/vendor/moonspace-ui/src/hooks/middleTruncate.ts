import { glyphs } from '../theme/glyphs.ts'

/**
 * Splits a string so it fits `budget` characters, keeping the start and the end.
 *
 * Pure and synchronous, so it is trivially testable and reusable — the planned TUI
 * renderer knows its width in columns already and can call this directly without
 * any of the measurement machinery below.
 */
export function middleTruncate(value: string, budget: number): string {
  const chars = [...value]
  if (budget <= 0) return ''
  if (chars.length <= budget) return value
  if (budget === 1) return glyphs.ellipsis

  const keep = budget - 1
  const head = Math.ceil(keep / 2)
  const tail = keep - head

  return `${chars.slice(0, head).join('')}${glyphs.ellipsis}${tail > 0 ? chars.slice(-tail).join('') : ''}`
}
