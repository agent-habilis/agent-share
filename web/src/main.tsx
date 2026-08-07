// First, before anything can build a Disposable. See the file for why.
import './compat.ts'

import { GlobalStyle, MoonspaceTheme } from 'moonspace-dom'
import { component, render } from 'visage-dom'

import './app.css'

import { App } from './App.tsx'
import { loadWasm } from './wasm.ts'

// Start the wasm fetch+compile now rather than when a session mounts — it is
// the largest asset on the connect path, and the promise memo in `wasm.ts`
// makes the later real call free. Its `.catch` reset means a failed eager
// load cannot poison that call either.
void loadWasm()

const root = document.getElementById('root')
if (!root) throw new Error('#root is missing from index.html')

const Root = component(function* () {
  yield () => (
    <>
      {MoonspaceTheme()}
      {GlobalStyle()}
      <App />
    </>
  )
})

render(<Root />, root)
