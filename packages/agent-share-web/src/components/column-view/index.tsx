/**
 * A macOS Finder column browser.
 *
 * One pane per level of the selected path, panes side by side on a single
 * raised surface, scrolling right as you descend. Column widths are
 * drag-resizable on the right border; double-clicking a border snaps that pane
 * to the width of its longest name — the same gestures Finder uses.
 *
 * Built from moonspace primitives — the design system has no column
 * component, and adding one there would mean designing for its eventual TUI
 * port, which this does not need.
 *
 * Hierarchy comes from colour, weight and inversion, never size: moonspace
 * has no `fontSize` token by design, and that *is* the "one size only"
 * requirement rather than a limitation to work around.
 */

import { Button, Stack, Text, MiddleTruncate, t } from 'moonspace-dom'
import { glyphs } from 'moonspace'
import { component, keyed, signal } from 'visage-dom'
import { Style, css } from 'visage-style'

import { seedLabel, seedState } from '../../lib/seeding/index.ts'
import { humanBytes, type DirNode, type Node } from '../../lib/tree.ts'

const DEFAULT_WIDTH = 28
const MIN_WIDTH = 12
/** Trailing chrome inside a row: the seed dot slot + the chevron slot. */
const ROW_CHROME = 2
/**
 * One trailing cell. Fixed width so the column holds whether or not the row
 * fills it; centred so a glyph wider than a cell overflows either side and its
 * centre, which is what the eye tracks down the column, stays put.
 */
const SLOT = { flex: 'none', width: '1ch', textAlign: 'center' } as const
/**
 * The gap keeps the chevron from reading as one compound glyph with the seed
 * dot beside it. The row's own trailing padding is what holds it off the column
 * divider.
 */
const CHEVRON_SLOT = { ...SLOT, marginLeft: '1ch' } as const
/**
 * Darker than the raised surface so rules stay quiet. Semantic `border` reads
 * too bright against `bg`.
 */
const SURFACE_BORDER = t.bgSunken

interface ColumnViewProps {
  root: DirNode
  /** Names from the root down to the selection, one per level. */
  path: string[]
  onPathChange: (path: string[]) => void
  onDownload: () => void
  downloadDisabled?: boolean
  /** Manifest indices this tab holds in full, and can seed. */
  held: ReadonlySet<number>
  /** Fraction of each partially-held file. See `seeding.ts`. */
  coverage?: ReadonlyMap<number, number>
  /** Pull the current selection into local storage, so this tab can seed it. */
  onSeed: () => void
  seedDisabled?: boolean
  /** Open the selected file in the preview view. Files only. */
  onPreview: () => void
  previewDisabled?: boolean
}

/** The directory chain the current path selects, root first. */
function columnsFor(root: DirNode, path: string[]): DirNode[] {
  const columns: DirNode[] = [root]
  let current = root
  for (const name of path) {
    const next = current.children.find(
      (child): child is DirNode => child.kind === 'dir' && child.name === name,
    )
    if (!next) break
    columns.push(next)
    current = next
  }
  return columns
}

/** The node the last path segment names. */
function selectedNode(columns: DirNode[], path: string[]): Node | undefined {
  const last = path[path.length - 1]
  if (last === undefined) return undefined
  const parent = columns[path.length - 1]
  return parent?.children.find((child) => child.name === last)
}

function scrollFollowRight(scroller: HTMLElement | null): void {
  requestAnimationFrame(() => {
    if (scroller) scroller.scrollLeft = scroller.scrollWidth
  })
}

/** Width of one `ch` in CSS pixels, measured in the live font. */
function measureCh(el: Element): number {
  const probe = document.createElement('span')
  probe.style.cssText =
    'position:absolute;visibility:hidden;white-space:pre;font:inherit;letter-spacing:inherit;'
  probe.textContent = '0'.repeat(100)
  el.appendChild(probe)
  const width = probe.getBoundingClientRect().width / 100
  probe.remove()
  return width > 0 ? width : 8
}

/**
 * The trailing gutter this column's rows carry, in `ch`.
 *
 * It exists to hold the disclosure chevron off the column divider, so a column
 * of files alone has no chevron and needs none — their empty chevron slot
 * already stands them clear of the edge.
 *
 * Decided per *column*, never per row: rows in one column must agree, or the
 * seed marks stop lining up, which is the one thing the fixed slot widths above
 * exist to guarantee. Returned as a number so the CSS and `fitColumnWidth`
 * share it rather than each carrying their own copy of the rule.
 */
function trailingGutter(dir: DirNode): number {
  return dir.children.some((child) => child.kind === 'dir') ? 1 : 0
}

function fitColumnWidth(dir: DirNode, padX: number): number {
  let longest = 0
  for (const child of dir.children) {
    if (child.name.length > longest) longest = child.name.length
  }
  return Math.max(MIN_WIDTH, longest + ROW_CHROME + trailingGutter(dir) + padX * 2)
}

/**
 * Width of a run of buttons, in cells.
 *
 * Each label plus the button's 1ch of padding on each side, and one cell of gap
 * between them. Exact: the button's edge is an outline, so it adds nothing.
 */
function buttonRow(labels: string[]): number {
  return labels.reduce((sum, label) => sum + label.length + 2, 0) + (labels.length - 1)
}

const FILE_BUTTONS = buttonRow(['download', 'seed', 'preview'])
const DIR_BUTTONS = buttonRow(['download', 'seed'])

function fitDetailWidth(node: Node, padX: number): number {
  if (node.kind !== 'file') {
    return Math.max(MIN_WIDTH, Math.max(node.name.length, DIR_BUTTONS) + padX * 2)
  }
  const size = humanBytes(node.size)
  const date = node.mtime > 0 ? new Date(node.mtime * 1000).toISOString().slice(0, 10) : ''
  const longest = Math.max(node.name.length, size.length, date.length, FILE_BUTTONS)
  return Math.max(MIN_WIDTH, longest + padX * 2)
}

export const ColumnView = component<ColumnViewProps>(function* (props) {
  /** Width in `ch` per column depth; missing entries use DEFAULT_WIDTH. */
  const widths = signal<number[]>([])
  const detailWidth = signal(DEFAULT_WIDTH)
  /** Suppresses the clear-on-click that would otherwise fire after a drag. */
  let suppressClear = false
  let scroller: HTMLDivElement | null = null

  function widthAt(depth: number): number {
    return widths.peek()[depth] ?? DEFAULT_WIDTH
  }

  function setWidth(depth: number, next: number): void {
    const clamped = Math.max(MIN_WIDTH, Math.round(next))
    const copy = widths.peek().slice()
    while (copy.length <= depth) copy.push(DEFAULT_WIDTH)
    copy[depth] = clamped
    widths.value = copy
  }

  function beginResize(
    event: MouseEvent,
    current: number,
    apply: (next: number) => void,
  ): void {
    event.preventDefault()
    event.stopPropagation()
    const handle = event.currentTarget as HTMLElement
    const startX = event.clientX
    const startW = current
    const ch = measureCh(handle)
    let dragged = false

    const onMove = (move: MouseEvent) => {
      const delta = (move.clientX - startX) / ch
      if (Math.abs(delta) >= 0.25) dragged = true
      apply(startW + delta)
    }
    const onUp = () => {
      window.removeEventListener('mousemove', onMove)
      window.removeEventListener('mouseup', onUp)
      if (dragged) {
        suppressClear = true
        requestAnimationFrame(() => {
          suppressClear = false
        })
      }
    }
    window.addEventListener('mousemove', onMove)
    window.addEventListener('mouseup', onUp)
  }

  yield () => {
    const root = props.root
    const path = props.path
    const columns = columnsFor(root, path)
    // Touch the widths signal so a drag re-renders the panes.
    const widthList = widths.value
    const detailW = detailWidth.value

    const writePath = (next: string[]) => {
      props.onPathChange(next)
      scrollFollowRight(scroller)
    }

    const select = (depth: number, node: Node) => {
      // Selecting in a column truncates everything to its right — the panes
      // past it described a path that is no longer current.
      writePath([...path.slice(0, depth), node.name])
    }

    const clearTo = (depth: number) => {
      if (suppressClear) return
      writePath(path.slice(0, depth))
    }

    // One raised surface for the whole browser — columns share it and only
    // draw a vertical rule between panes, rather than each boxing itself.
    return (
      <div
        ref={(el) => {
          scroller = el as HTMLDivElement
          scrollFollowRight(scroller)
        }}
        onclick={() => {
          if (!suppressClear) writePath([])
        }}
        style={{
          display: 'flex',
          overflowX: 'auto',
          alignItems: 'stretch',
          flex: 1,
          minHeight: 0,
          background: t.bg,
          outline: `1px solid ${SURFACE_BORDER}`,
          outlineOffset: 0,
        }}
      >
        {keyed(columns, (column) => column.path || '/', (column, depth) => {
          const width = widthList[depth] ?? DEFAULT_WIDTH
          return (
            <Column
              dir={column}
              selected={path[depth]}
              width={width}
              padX={2}
              onSelect={(node) => select(depth, node)}
              onClear={() => clearTo(depth)}
              onResizeStart={(event) =>
                beginResize(event, widthAt(depth), (next) => setWidth(depth, next))
              }
              onFit={() => setWidth(depth, fitColumnWidth(column, 2))}
              held={props.held}
              coverage={props.coverage}
              onPreview={props.onPreview}
              previewDisabled={props.previewDisabled}
            />
          )
        })}
        <Detail
          node={selectedNode(columns, path)}
          width={detailW}
          onDownload={props.onDownload}
          downloadDisabled={props.downloadDisabled}
          onResizeStart={(event) =>
            beginResize(event, detailWidth.peek(), (next) => {
              detailWidth.value = Math.max(MIN_WIDTH, Math.round(next))
            })
          }
          onFit={(node) => {
            detailWidth.value = fitDetailWidth(node, 2)
          }}
          held={props.held}
              coverage={props.coverage}
          onSeed={props.onSeed}
          seedDisabled={props.seedDisabled}
          onPreview={props.onPreview}
          previewDisabled={props.previewDisabled}
        />
      </div>
    )
  }
})

function ResizeHandle({
  onResizeStart,
  onFit,
}: {
  onResizeStart: (event: MouseEvent) => void
  onFit: () => void
}) {
  return (
    <div
      role="separator"
      aria-orientation="vertical"
      aria-label="Resize column"
      onmousedown={onResizeStart}
      ondblclick={(event: MouseEvent) => {
        event.preventDefault()
        event.stopPropagation()
        onFit()
      }}
      onclick={(event: MouseEvent) => event.stopPropagation()}
      style={{
        position: 'absolute',
        top: 0,
        right: -3,
        width: '6px',
        height: '100%',
        cursor: 'col-resize',
        zIndex: 2,
      }}
    />
  )
}

function Column({
  dir,
  selected,
  width,
  padX,
  onSelect,
  onClear,
  onResizeStart,
  onFit,
  held,
  coverage,
  onPreview,
  previewDisabled,
}: {
  dir: DirNode
  selected: string | undefined
  width: number
  padX: number
  onSelect: (node: Node) => void
  onClear: () => void
  onResizeStart: (event: MouseEvent) => void
  onFit: () => void
  held: ReadonlySet<number>
  /** Fraction of each partially-held file. See `seeding.ts`. */
  coverage?: ReadonlyMap<number, number>
  onPreview: () => void
  previewDisabled?: boolean
}) {
  return (
    <div
      onclick={(event: MouseEvent) => {
        event.stopPropagation()
        onClear()
      }}
      style={{
        position: 'relative',
        height: '100%',
        flexShrink: 0,
        display: 'flex',
        flexDirection: 'column',
        boxSizing: 'border-box',
        width: `${width}ch`,
        borderRight: `2px solid ${SURFACE_BORDER}`,
      }}
    >
      <div style={{ overflowY: 'auto', flex: 1, minHeight: 0, paddingBottom: 'var(--bottom-inset)' }}>
        {Style(ROWS)}
        {dir.children.length === 0 ? (
          <div style={{ padding: `0 ${padX}ch` }}>
            <Text color="fgSubtle">(empty)</Text>
          </div>
        ) : (
          keyed(dir.children, (child) => child.path, (child) => (
            <Row
              node={child}
              active={child.name === selected}
              padX={padX}
              gutter={trailingGutter(dir)}
              onSelect={() => onSelect(child)}
              held={held}
              coverage={coverage}
              onPreview={onPreview}
              previewDisabled={previewDisabled}
            />
          ))
        )}
      </div>
      <ResizeHandle onResizeStart={onResizeStart} onFit={onFit} />
    </div>
  )
}

/*
 * A rule rather than an inline `background`, which would beat `:hover`; a row
 * is an unfilled control on the page background, so it takes the fill a ghost
 * button takes. Hung on the column, not the row, because `@scope` binds to the
 * `<style>`'s parent and a column renders every child the directory has.
 * Matching `"false"` rather than negating `"true"` keeps the hover off every
 * descendant that carries no state at all.
 *
 * Focus takes the hover fill instead of the browser's ring, which the scroll
 * body clips to two lines. While the pointer is on any row that row is the
 * current one, so the focused row gives its fill up until the pointer leaves.
 */
const ROWS = css({
  '[data-active="true"]': { background: t.bgSelected },
  '[data-active="false"]:hover': { background: t.bgRaised },
  '[data-active]:focus': { outline: 'none' },
  '[data-active="false"]:focus-visible': { background: t.bgRaised },
  '&:has([data-active]:hover) [data-active="false"]:focus-visible:not(:hover)': {
    background: 'transparent',
  },
})

/*
 * Written through `dataset`, which stringifies, rather than as a bare
 * `data-active`: visage-dom gives a raw attribute HTML boolean semantics and
 * writes `true` as the empty string, which `[data-active="true"]` never
 * matches. Two constants rather than a fresh object, so a row whose state has
 * not moved is `Object.is` to its last render and skips the dataset diff —
 * every open row re-renders on the one-second holdings repaint.
 */
const ACTIVE = Object.freeze({ active: 'true' })
const INACTIVE = Object.freeze({ active: 'false' })

function Row({
  node,
  active,
  padX,
  gutter,
  onSelect,
  held,
  coverage,
  onPreview,
  previewDisabled,
}: {
  node: Node
  active: boolean
  padX: number
  /** Trailing gutter in `ch`, from [`trailingGutter`]. */
  gutter: number
  onSelect: () => void
  held: ReadonlySet<number>
  /** Fraction of each partially-held file. See `seeding.ts`. */
  coverage?: ReadonlyMap<number, number>
  onPreview: () => void
  previewDisabled?: boolean
}) {
  const state = seedState(node, held, coverage)
  return (
    <div
      role="button"
      tabIndex={0}
      onclick={(event: MouseEvent) => {
        event.stopPropagation()
        onSelect()
      }}
      /*
        Double-click opens the file, the way it does in Finder. Files only:
        a folder already opens on the first click, since that is what pushes
        its column into view.

        No `onSelect()` here — the two clicks underneath this one have already
        run, so the row is the selection by the time this fires, which is what
        `onPreview` reads.
      */
      ondblclick={(event: MouseEvent) => {
        if (node.kind !== 'file' || previewDisabled) return
        event.stopPropagation()
        onPreview()
      }}
      onkeydown={(event: KeyboardEvent) => {
        if (event.key === 'Enter' || event.key === ' ') {
          event.preventDefault()
          onSelect()
        }
      }}
      style={{
        display: 'flex',
        alignItems: 'center',
        width: '100%',
        boxSizing: 'border-box',
        cursor: 'pointer',
        // Full-bleed highlight; the name keeps the left gutter, and a column
        // holding folders keeps one on the right so the chevron clears the
        // divider instead of touching it. Padding rather than a margin on the
        // chevron: both marks are flex children here, so one value moves them
        // together and a file's empty chevron slot stays aligned with a
        // folder's by construction.
        paddingLeft: `${padX}ch`,
        paddingRight: `${gutter}ch`,
      }}
      dataset={active ? ACTIVE : INACTIVE}
    >
      <div style={{ minWidth: 0, flex: 1, overflow: 'hidden' }}>
        <Text weight={node.kind === 'dir' ? 'bold' : 'regular'}>
          <MiddleTruncate value={node.name} />
        </Text>
      </div>
      {/*
        A fixed one-character slot, not a bare glyph: the geometric marks are
        outside the monospace face's core and their advance is not guaranteed,
        so the width is pinned here and the glyph centred in it. The dot then
        holds its place as a sync lands, and the name column never reflows.

        The shape says *how much* and the colour says *whether*: `◐` against `●`
        is partial against full, while accent against subtle is holding
        something against holding nothing. Partial used to be subtle too, which
        gave it the same colour as a node holding nothing — the one distinction
        the mark exists to draw. Still no third colour: a folder mid-sync is
        progress, not a fault.
      */}
      <div style={SLOT}>
        <Text
          color={state === 'none' ? 'fgSubtle' : 'accent'}
          title={seedLabel(state, node, held, coverage)}
        >
          {state === 'full' ? '\u25cf' : state === 'partial' ? '\u25d0' : '\u00b7'}
        </Text>
      </div>
      {/* Files keep the empty slot so their dot lands in the same column as a
          folder's, rather than jogging right into the vacated chevron cell. */}
      <div style={CHEVRON_SLOT}>
        {node.kind === 'dir' ? (
          <Text color={active ? 'fg' : 'fgSubtle'}>{glyphs.chevron.right}</Text>
        ) : null}
      </div>
    </div>
  )
}

/** The rightmost pane: metadata and actions for the current selection. */
function Detail({
  node,
  width,
  onDownload,
  downloadDisabled,
  onResizeStart,
  onFit,
  held,
  coverage,
  onSeed,
  seedDisabled,
  onPreview,
  previewDisabled,
}: {
  node: Node | undefined
  width: number
  onDownload: () => void
  downloadDisabled?: boolean
  onResizeStart: (event: MouseEvent) => void
  onFit: (node: Node) => void
  held: ReadonlySet<number>
  /** Fraction of each partially-held file. See `seeding.ts`. */
  coverage?: ReadonlyMap<number, number>
  onSeed: () => void
  seedDisabled?: boolean
  onPreview: () => void
  previewDisabled?: boolean
}) {
  if (!node) return null
  const state = seedState(node, held, coverage)
  return (
    <div
      onclick={(event: MouseEvent) => event.stopPropagation()}
      style={{
        position: 'relative',
        height: '100%',
        flexShrink: 0,
        display: 'flex',
        flexDirection: 'column',
        boxSizing: 'border-box',
        width: `${width}ch`,
        padding: '0 2ch',
        borderRight: `2px solid ${SURFACE_BORDER}`,
      }}
    >
      <div style={{ overflowY: 'auto', flex: 1, minHeight: 0, paddingBottom: 'var(--bottom-inset)' }}>
        <Stack direction="column" gap={1}>
          <Text weight="bold">
            <MiddleTruncate value={node.name} />
          </Text>
          {node.kind === 'file' ? (
            <>
              <Text color="fgMuted">{humanBytes(node.size)}</Text>
              {node.mtime > 0 ? (
                <Text color="fgSubtle">
                  {new Date(node.mtime * 1000).toISOString().slice(0, 10)}
                </Text>
              ) : null}
            </>
          ) : (
            <Text color="fgMuted">folder</Text>
          )}
          {/*
            Wrapping, because three buttons are wider than the pane's default
            width and the pane is the one column here that cannot scroll
            sideways — without it the last button is simply cut off. Fitting
            the pane (double-click the border) still snaps to a width that
            holds them on one line.
          */}
          <div style={{ alignSelf: 'start' }}>
            <Stack direction="row" gap={1} wrap>
              <Button
                variant="secondary"
                onclick={() => onDownload()}
                disabled={downloadDisabled}
              >
                Download
              </Button>
              {/*
                Disabled once everything here is held: pressing it again would
                be a no-op the client skips anyway, and a button that does
                nothing is worse than one that says it has nothing to do.

                Still `Seed` rather than `Seeding` while `seedDisabled` holds —
                that flag also covers redialling, so it does not mean a seed is
                running, and only the held state can claim so honestly.
              */}
              <Button
                variant="secondary"
                onclick={() => onSeed()}
                disabled={seedDisabled || state === 'full'}
              >
                {state === 'full' ? 'Seeding' : 'Seed'}
              </Button>
              {/*
                Files only. A folder has no single thing to render, and a
                button that opens a view saying so is worse than no button.
              */}
              {node.kind === 'file' ? (
                <Button
                  variant="secondary"
                  onclick={() => onPreview()}
                  disabled={previewDisabled}
                >
                  Preview
                </Button>
              ) : null}
            </Stack>
          </div>
        </Stack>
      </div>
      <ResizeHandle onResizeStart={onResizeStart} onFit={() => onFit(node)} />
    </div>
  )
}
