/**
 * A macOS Finder column browser.
 *
 * One pane per level of the selected path, panes side by side, scrolling right
 * as you descend. Built from moonspace-ui primitives — the design system has no
 * column component, and adding one there would mean designing for its eventual
 * TUI port, which this does not need.
 *
 * Hierarchy comes from colour, weight and inversion, never size: moonspace-ui
 * has no `fontSize` token by design, and that *is* the "one size only"
 * requirement rather than a limitation to work around.
 */

import { Box, Stack, Text, MiddleTruncate, glyphs, theme } from 'moonspace-ui'
import { useCallback, useEffect, useRef } from 'react'

import { humanBytes, type DirNode, type Node } from './tree.ts'

const COLUMN_WIDTH = 28

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

export function ColumnView({ root, path, onPathChange }: ColumnViewProps) {
  const columns = columnsFor(root, path)
  const scroller = useRef<HTMLDivElement>(null)

  // Follow the selection rightwards, the way Finder does.
  useEffect(() => {
    const element = scroller.current
    if (element) element.scrollLeft = element.scrollWidth
  }, [path.length])

  const select = useCallback(
    (depth: number, node: Node) => {
      // Selecting in a column truncates everything to its right — the panes
      // past it described a path that is no longer current.
      onPathChange([...path.slice(0, depth), node.name])
    },
    [path, onPathChange],
  )

  return (
    <div
      ref={scroller}
      style={{ display: 'flex', overflowX: 'auto', alignItems: 'stretch', flex: 1 }}
    >
      {columns.map((column, depth) => (
        <Column
          key={column.path || '/'}
          dir={column}
          selected={path[depth]}
          onSelect={(node) => select(depth, node)}
        />
      ))}
      <Detail node={selectedNode(columns, path)} />
    </div>
  )
}

/** The node the last path segment names, if it is a file. */
function selectedNode(columns: DirNode[], path: string[]): Node | undefined {
  const last = path[path.length - 1]
  if (last === undefined) return undefined
  const parent = columns[path.length - 1]
  return parent?.children.find((child) => child.name === last)
}

function Column({
  dir,
  selected,
  onSelect,
}: {
  dir: DirNode
  selected: string | undefined
  onSelect: (node: Node) => void
}) {
  return (
    <Box border="line" padX={1} padY={0} width={COLUMN_WIDTH}>
      <div style={{ overflowY: 'auto', maxHeight: '70vh' }}>
        {dir.children.length === 0 ? (
          <Text color="fgSubtle">(empty)</Text>
        ) : (
          dir.children.map((child) => (
            <Row
              key={child.path}
              node={child}
              active={child.name === selected}
              onSelect={() => onSelect(child)}
            />
          ))
        )}
      </div>
    </Box>
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
      onClick={onSelect}
      onKeyDown={(event) => {
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
        <MiddleTruncate value={node.name} budget={COLUMN_WIDTH - 3} />
        <Text color={active ? 'fg' : 'fgSubtle'}>
          {node.kind === 'dir' ? glyphs.chevron.right : ' '}
        </Text>
      </Stack>
    </div>
  )
}

/** The rightmost pane: what the selected file is, when one is selected. */
function Detail({ node }: { node: Node | undefined }) {
  if (!node || node.kind !== 'file') return null
  return (
    <Box border="line" padX={1} padY={0} width={COLUMN_WIDTH}>
      <Stack direction="column" gap={1}>
        <Text weight="bold">
          <MiddleTruncate value={node.name} budget={COLUMN_WIDTH - 2} />
        </Text>
        <Text color="fgMuted">{humanBytes(node.size)}</Text>
        {node.mtime > 0 && (
          <Text color="fgSubtle">{new Date(node.mtime * 1000).toISOString().slice(0, 10)}</Text>
        )}
      </Stack>
    </Box>
  )
}
