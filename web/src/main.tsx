// First, before anything can build a Disposable. See the file for why.
import './compat.ts'

import { T, Theme } from 'moonspace-ui'
import { component, render } from 'visage-dom'

import 'moonspace-ui/src/global.css'
import './app.css'

import { App } from './App.tsx'

const root = document.getElementById('root')
if (!root) throw new Error('#root is missing from index.html')

const Root = component(function* () {
  yield () => (
    <>
      {Theme(T)}
      <App />
    </>
  )
})

render(<Root />, root)
