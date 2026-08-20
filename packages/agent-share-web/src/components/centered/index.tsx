import type { Child } from 'visage-dom'

export function Centered({ children }: { children: Child }) {
  return (
    <div
      style={{
        flex: 1,
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
        minHeight: 0,
        width: '100%',
      }}
    >
      {children}
    </div>
  )
}
