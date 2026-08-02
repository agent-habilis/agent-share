import type { Child, Key } from 'visage-dom'
import { keyed } from 'visage-dom/element'
import { Style, css, raw } from 'visage-style'
import { oneRow } from '../../styles/mixins.ts'
import { T } from '../../tokens.ts'

export interface Column<T> {
  key: keyof T & string
  header: Child
  width?: number
  align?: 'left' | 'right'
  render?: (row: T) => Child
}

export interface TableProps<T> {
  columns: Column<T>[]
  rows: T[]
  rowKey: (row: T) => Key
  headerRule?: boolean
  rowRules?: boolean
  class?: string
  caption?: string
}

const GRID = css({
  display: 'grid',
  columnGap: '1ch',
  width: '100%',
  '@supports (width: round(down, 100%, 1ch))': {
    width: raw('round(down, 100%, 1ch)'),
  },
} as never)

const CELL = css({
  ...oneRow,
  overflow: 'hidden',
  whiteSpace: 'nowrap',
  minWidth: 0,
} as never)

const HEADER = css({
  color: T.msFgMuted,
  textTransform: 'uppercase',
} as never)

const RULE = css({
  gridColumn: raw('1 / -1'),
  display: 'flex',
  alignItems: 'center',
  height: 'var(--ms-row)',
  '&::before': {
    content: '""',
    flex: 1,
    borderTop: raw('1px solid var(--ms-border)'),
  },
} as never)

const CAPTION = css({
  ...oneRow,
  color: T.msFgSubtle,
} as never)

export function Table<T>({
  columns,
  rows,
  rowKey,
  headerRule = true,
  rowRules = false,
  class: className,
  caption,
}: TableProps<T>) {
  const template = columns
    .map((column) => (column.width !== undefined ? `${column.width}ch` : 'minmax(0, 1fr)'))
    .join(' ')

  return (
    <div {...(className !== undefined ? { class: className } : {})}>
      {caption != null && (
        <div>
          {Style(CAPTION)}
          {caption}
        </div>
      )}
      <div role="table" style={{ gridTemplateColumns: template }}>
        {Style(GRID)}
        {columns.map((column) => (
          <div
            key={column.key}
            role="columnheader"
            style={{ textAlign: column.align ?? 'left' }}
          >
            {Style(CELL)}
            {Style(HEADER)}
            {column.header}
          </div>
        ))}

        {headerRule && (
          <div aria-hidden="true">
            {Style(RULE)}
          </div>
        )}

        {keyed(rows, rowKey, (row, index) => (
          <>
            {rowRules && index > 0 && (
              <div aria-hidden="true">
                {Style(RULE)}
              </div>
            )}
            {columns.map((column) => (
              <div key={column.key} role="cell" style={{ textAlign: column.align ?? 'left' }}>
                {Style(CELL)}
                {column.render ? column.render(row) : String(row[column.key] ?? '')}
              </div>
            ))}
          </>
        ))}
      </div>
    </div>
  )
}
