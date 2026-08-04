/**
 * A macOS Finder column browser.
 *
 * One pane per level of the selected path, panes side by side on a single
 * raised surface, scrolling right as you descend. Column widths are
 * drag-resizable on the right border; double-clicking a border snaps that pane
 * to the width of its longest name — the same gestures Finder uses.
 *
 * Built from moonspace-ui primitives — the design system has no column
 * component, and adding one there would mean designing for its eventual TUI
 * port, which this does not need.
 *
 * Hierarchy comes from colour, weight and inversion, never size: moonspace-ui
 * has no `fontSize` token by design, and that *is* the "one size only"
 * requirement rather than a limitation to work around.
 */

import { Button, Stack, Text, MiddleTruncate, glyphs, roleVar, theme } from 'moonspace-ui'
import { component, keyed, signal } from 'visage-dom'

import { seedLabel, seedState } from './seeding.ts'
import { humanBytes, type DirNode, type Node } from './tree.ts'

const DEFAULT_WIDTH = 28
const MIN_WIDTH = 12
/** Trailing chrome inside a row: chevron + nbsp. */
const ROW_CHROME = 2
/**
 * Darker than the raised surface so rules stay quiet. Semantic `border` reads
 * too bright against `bg`.
 */
const SURFACE_BORDER = roleVar.bgSunken

interface ColumnViewProps {
  root: DirNode
  /** Names from the root down to the selection, one per level. */
  path: string[]
  onPathChange: (path: string[]) => void
  onDownload: () => void
  downloadDisabled?: boolean
  /** Manifest indices this tab holds in full, and can seed. */
  held: ReadonlySet<number>
  /** Pull the current selection into local storage. */
  onSync: () => void
  syncDisabled?: boolean
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

function fitColumnWidth(dir: DirNode, padX: number): number {
  let longest = 0
  for (const child of dir.children) {
    if (child.name.length > longest) longest = child.name.length
  }
  return Math.max(MIN_WIDTH, longest + ROW_CHROME + padX * 2)
}

function fitDetailWidth(node: Node, padX: number): number {
  // "Download" label plus brackets from the primary button chrome.
  const downloadLabel = 10
  if (node.kind !== 'file') {
    return Math.max(MIN_WIDTH, Math.max(node.name.length, downloadLabel) + padX * 2)
  }
  const size = humanBytes(node.size)
  const date = node.mtime > 0 ? new Date(node.mtime * 1000).toISOString().slice(0, 10) : ''
  const longest = Math.max(node.name.length, size.length, date.length, downloadLabel)
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
          background: roleVar.bg,
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
          onSync={props.onSync}
          syncDisabled={props.syncDisabled}
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
      <div style={{ overflowY: 'auto', flex: 1, minHeight: 0 }}>
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
              onSelect={() => onSelect(child)}
              held={held}
            />
          ))
        )}
      </div>
      <ResizeHandle onResizeStart={onResizeStart} onFit={onFit} />
    </div>
  )
}

function Row({
  node,
  active,
  padX,
  onSelect,
  held,
}: {
  node: Node
  active: boolean
  padX: number
  onSelect: () => void
  held: ReadonlySet<number>
}) {
  const state = seedState(node, held)
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
        display: 'flex',
        alignItems: 'center',
        width: '100%',
        boxSizing: 'border-box',
        cursor: 'pointer',
        // Full-bleed highlight; name keeps the left gutter. Chevron sits on the
        // trailing edge like Finder column view.
        paddingLeft: `${padX}ch`,
        paddingRight: '1ch',
        background: active ? theme.color.bgSelected : 'transparent',
      }}
    >
      <div style={{ minWidth: 0, flex: 1, overflow: 'hidden' }}>
        <Text weight={node.kind === 'dir' ? 'bold' : 'regular'}>
          <MiddleTruncate value={node.name} />
        </Text>
      </div>
      {/*
        One character wide whatever the state, so the name column never
        reflows as a sync lands. A hollow mark for partial rather than a
        second colour: the difference that matters is held or not, and a
        folder mid-sync should not read as an error.
      */}
      <Text
        color={state === 'full' ? 'accent' : 'fgSubtle'}
        title={seedLabel(state, node, held)}
      >
        {state === 'full' ? '\u25cf' : state === 'partial' ? '\u25d0' : '\u00b7'}
      </Text>
      {node.kind === 'dir' ? (
        <Text color={active ? 'fg' : 'fgSubtle'}>{glyphs.chevron.right}</Text>
      ) : null}
    </div>
  )
}

/** The rightmost pane: metadata and download for the current selection. */
function Detail({
  node,
  width,
  onDownload,
  downloadDisabled,
  onResizeStart,
  onFit,
  held,
  onSync,
  syncDisabled,
}: {
  node: Node | undefined
  width: number
  onDownload: () => void
  downloadDisabled?: boolean
  onResizeStart: (event: MouseEvent) => void
  onFit: (node: Node) => void
  held: ReadonlySet<number>
  onSync: () => void
  syncDisabled?: boolean
}) {
  if (!node) return null
  const state = seedState(node, held)
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
      <div style={{ overflowY: 'auto', flex: 1, minHeight: 0 }}>
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
            Stated in words as well as by the row mark. "seeding" rather than
            "downloaded": holding the bytes is not the interesting part, other
            people being able to get them from you is.
          */}
          <Text color={state === 'full' ? 'accent' : 'fgSubtle'}>
            {seedLabel(state, node, held)}
          </Text>
          <div style={{ alignSelf: 'start' }}>
            <Stack direction="row" gap={1}>
              <Button
                variant="primary"
                onclick={() => onDownload()}
                disabled={downloadDisabled}
              >
                Download
              </Button>
              {/*
                Disabled once everything here is held: pressing it again would
                be a no-op the client skips anyway, and a button that does
                nothing is worse than one that says it has nothing to do.
              */}
              <Button
                variant="secondary"
                onclick={() => onSync()}
                disabled={syncDisabled || state === 'full'}
              >
                {state === 'full' ? 'Synced' : 'Sync'}
              </Button>
            </Stack>
          </div>
        </Stack>
      </div>
      <ResizeHandle onResizeStart={onResizeStart} onFit={() => onFit(node)} />
    </div>
  )
}
