// First, before anything can build a Disposable. See the file for why.
import './compat.ts'

import { GlobalStyle, MoonspaceTheme } from 'moonspace-dom'
import { component, render } from 'visage-dom'

import './app.css'

import { registerAgentTools } from './lib/agentTools/index.ts'
import { App } from './pages/index.ts'
import { loadWasm } from './wasm/index.ts'

// Start the wasm fetch+compile now rather than when a session mounts — it is
// the largest asset on the connect path, and the promise memo in `wasm.ts`
// makes the later real call free. Its `.catch` reset means a failed eager
// load cannot poison that call either.
void loadWasm()

// Publish the page's tools to an agent, if this browser speaks WebMCP. A no-op
// on every browser that does not, which is currently all of them by default —
// so it is not worth waiting for, and not worth failing the page over.
void registerAgentTools().catch((error: unknown) => {
  console.debug('[agent-share] publishing agent tools failed', error)
})

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
