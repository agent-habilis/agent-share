/**
 * A share's slots as a row of squares, the way a BitTorrent client paints
 * pieces.
 *
 * A square is one *manifest slot* — one file — because that is the granularity
 * this protocol addresses bytes with, and therefore the granularity a peer can
 * honestly answer for. Per-chunk squares would mean republishing availability
 * on every range that arrives, onto a CRDT that keeps its history.
 *
 * Three states rather than two. `partial` is the one that earns its place:
 * chunks are addressed and verified individually, so a peer holding part of a
 * file genuinely serves that part, and drawing it as empty would say a useful
 * source is useless.
 */

import { t } from 'moonspace-dom'

import type { SeedState } from 'agent-share-core/seeding'

export interface SlotGridProps {
  states: readonly SeedState[]
  /**
   * Wrap every `columns` squares, making a block of a known shape.
   *
   * Left off, the squares run on one line — which is what a table cell, exactly
   * one row tall, needs.
   */
  columns?: number
}

/**
 * Squares are `0.8ch` rather than a whole cell so a wrapped grid reads as a
 * field rather than as a line of text, and the 1px gap keeps two held
 * neighbours from merging into one bar.
 */
const SIZE = '0.8ch'

function fill(state: SeedState): string {
  if (state === 'full') return String(t.accent)
  // The partial square has to be legible as "some of it" at 0.8ch, where a
  // hatch or a half-fill is invisible — so it is the held colour, dimmed.
  if (state === 'partial') return `color-mix(in srgb, ${t.accent} 45%, ${t.bgSunken})`
  return String(t.bgSunken)
}

export function SlotGrid(props: SlotGridProps) {
  return (
    <div
      style={
        props.columns
          ? {
              display: 'grid',
              // `max-content` rather than a fraction: the block is sized by its
              // squares, so it stays square whatever the panel around it does.
              gridTemplateColumns: `repeat(${props.columns}, ${SIZE})`,
              gap: '1px',
              width: 'max-content',
            }
          : { display: 'flex', flexWrap: 'nowrap', gap: '1px', overflow: 'hidden' }
      }
    >
      {props.states.map((state, slot) => (
        <span
          key={slot}
          title={`slot ${slot}: ${state === 'full' ? 'available' : state === 'partial' ? 'partial' : 'missing'}`}
          style={{
            flex: '0 0 auto',
            width: SIZE,
            height: SIZE,
            background: fill(state),
            outline: state === 'full' ? 'none' : `1px solid ${t.border}`,
          }}
        />
      ))}
    </div>
  )
}
