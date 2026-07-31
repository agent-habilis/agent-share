/**
 * A macOS Finder column browser.
 *
 * One pane per level of the selected path, panes side by side, scrolling right
 * as you descend. Column widths are drag-resizable on the right border;
 * double-clicking a border snaps that pane to the width of its longest name —
 * the same gestures Finder uses.
 *
 * Built from moonspace-ui primitives — the design system has no column
 * component, and adding one there would mean designing for its eventual TUI
 * port, which this does not need.
 *
 * Hierarchy comes from colour, weight and inversion, never size: moonspace-ui
 * has no `fontSize` token by design, and that *is* the "one size only"
 * requirement rather than a limitation to work around.
 */

import { Box, Stack, Text, MiddleTruncate, glyphs, theme } from 'moonspace-ui'
import { component, keyed, signal } from 'visage-dom'

import { humanBytes, type DirNode, type Node } from './tree.ts'

const DEFAULT_WIDTH = 28
const MIN_WIDTH = 12
/** Trailing chrome inside a row: chevron + nbsp. */
const ROW_CHROME = 2

interface ColumnViewProps {
  root: DirNode
  /** Names from the root down to the selection, one per level. */
  path: string[]
  onPathChange: (path: string[]) => void
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

/** The node the last path segment names, if it is a file. */
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

function fitColumnWidth(dir: DirNode, padX: number): number {
  let longest = 0
  for (const child of dir.children) {
    if (child.name.length > longest) longest = child.name.length
  }
  return Math.max(MIN_WIDTH, longest + ROW_CHROME + padX * 2)
}

function fitDetailWidth(node: Node, padX: number): number {
  if (node.kind !== 'file') return DEFAULT_WIDTH
  const size = humanBytes(node.size)
  const date = node.mtime > 0 ? new Date(node.mtime * 1000).toISOString().slice(0, 10) : ''
  const longest = Math.max(node.name.length, size.length, date.length)
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
        }}
      >
        {keyed(columns, (column) => column.path || '/', (column, depth) => {
          const padX = depth === 0 ? 0 : 1
          const width = widthList[depth] ?? DEFAULT_WIDTH
          return (
            <Column
              dir={column}
              selected={path[depth]}
              width={width}
              padX={padX}
              onSelect={(node) => select(depth, node)}
              onClear={() => clearTo(depth)}
              onResizeStart={(event) =>
                beginResize(event, widthAt(depth), (next) => setWidth(depth, next))
              }
              onFit={() => setWidth(depth, fitColumnWidth(column, padX))}
            />
          )
        })}
        <Detail
          node={selectedNode(columns, path)}
          width={detailW}
          onResizeStart={(event) =>
            beginResize(event, detailWidth.peek(), (next) => {
              detailWidth.value = Math.max(MIN_WIDTH, Math.round(next))
            })
          }
          onFit={(node) => {
            detailWidth.value = fitDetailWidth(node, 1)
          }}
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
}: {
  dir: DirNode
  selected: string | undefined
  width: number
  padX: number
  onSelect: (node: Node) => void
  onClear: () => void
  onResizeStart: (event: MouseEvent) => void
  onFit: () => void
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
        width: `${width}ch`,
      }}
    >
      <div style={{ flex: 1, minHeight: 0, display: 'flex' }}>
        <Box border="line" padX={padX} padY={0} width={width}>
          <div style={{ overflowY: 'auto', height: '100%' }}>
            {dir.children.length === 0 ? (
              <Text color="fgSubtle">(empty)</Text>
            ) : (
              keyed(dir.children, (child) => child.path, (child) => (
                <Row
                  node={child}
                  active={child.name === selected}
                  onSelect={() => onSelect(child)}
                />
              ))
            )}
          </div>
        </Box>
      </div>
      <ResizeHandle onResizeStart={onResizeStart} onFit={onFit} />
    </div>
  )
}

function Row({
  node,
  active,
  onSelect,
}: {
  node: Node
  active: boolean
  onSelect: () => void
}) {
  return (
    <div
      role="button"
      tabIndex={0}
      onclick={(event: MouseEvent) => {
        event.stopPropagation()
        onSelect()
      }}
      onkeydown={(event: KeyboardEvent) => {
        if (event.key === 'Enter' || event.key === ' ') {
          event.preventDefault()
          onSelect()
        }
      }}
      style={{
        cursor: 'pointer',
        background: active ? theme.color.bgSelected : 'transparent',
      }}
    >
      <Stack direction="row" gap={0} justify="between">
        <div style={{ minWidth: 0, flex: 1, overflow: 'hidden' }}>
          <MiddleTruncate value={node.name} />
        </div>
        <Text color={active ? 'fg' : 'fgSubtle'}>
          {node.kind === 'dir' ? `${glyphs.chevron.right}\u00a0` : '\u00a0\u00a0'}
        </Text>
      </Stack>
    </div>
  )
}

/** The rightmost pane: what the selected file is, when one is selected. */
function Detail({
  node,
  width,
  onResizeStart,
  onFit,
}: {
  node: Node | undefined
  width: number
  onResizeStart: (event: MouseEvent) => void
  onFit: (node: Node) => void
}) {
  if (!node || node.kind !== 'file') return null
  return (
    <div
      onclick={(event: MouseEvent) => event.stopPropagation()}
      style={{
        position: 'relative',
        height: '100%',
        flexShrink: 0,
        display: 'flex',
        flexDirection: 'column',
        width: `${width}ch`,
      }}
    >
      <div style={{ flex: 1, minHeight: 0, display: 'flex' }}>
        <Box border="line" padX={1} padY={0} width={width}>
          <div style={{ overflowY: 'auto', height: '100%' }}>
            <Stack direction="column" gap={1}>
              <Text weight="bold">
                <MiddleTruncate value={node.name} />
              </Text>
              <Text color="fgMuted">{humanBytes(node.size)}</Text>
              {node.mtime > 0 ? (
                <Text color="fgSubtle">
                  {new Date(node.mtime * 1000).toISOString().slice(0, 10)}
                </Text>
              ) : null}
            </Stack>
          </div>
        </Box>
      </div>
      <ResizeHandle onResizeStart={onResizeStart} onFit={() => onFit(node)} />
    </div>
  )
}
