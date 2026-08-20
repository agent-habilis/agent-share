/**
 * Lab: the workbench pages the app itself cannot be — a synthetic `OP_BENCH`
 * producer and consumer, a real file share built without a picker, and this tab
 * as a mesh peer.
 *
 * It wears the app's chrome and the app's bento (see `components/Panel`) rather
 * than a stylesheet of its own, so the two pages age together. It is still its
 * own HTML entrypoint, outside the router: nothing in the app links here, and
 * the harnesses in `tasks/` reach it at `/lab` by URL.
 */

// First, before anything can build a Disposable. See the file for why.
import '../../compat.ts'

import { GlobalStyle, MoonspaceTheme, t } from 'moonspace-dom'
import { component, render } from 'visage-dom'

import '../../app.css'

import { Chrome } from '../../components/Chrome/index.tsx'
import { Bento } from '../../components/Panel/index.tsx'
import { ConsumerPanel } from './consumer.tsx'
import { MeshPanel } from './mesh.tsx'
import { ProducerPanel } from './producer.tsx'
import { SharePanel } from './share.tsx'

const LabPage = component(function* () {
  yield () => (
    <Chrome crumb="lab">
      <div
        // The whole page is tickets, mesh ids and logs — text that exists to be
        // copied out. Selection is off app-wide (see `app.css`).
        class="selectable"
        style={{
          flex: 1,
          minHeight: 0,
          overflowY: 'auto',
          padding: 'var(--ms-row) 2ch calc(2 * var(--ms-row))',
          background: t.bg,
        }}
      >
        <Bento>
          <ProducerPanel />
          <SharePanel />
          <ConsumerPanel />
          <MeshPanel />
        </Bento>
      </div>
    </Chrome>
  )
})

const root = document.getElementById('root')
if (!root) throw new Error('#root is missing from index.html')

const Root = component(function* () {
  yield () => (
    <>
      {MoonspaceTheme()}
      {GlobalStyle()}
      <LabPage />
    </>
  )
})

render(<Root />, root)
