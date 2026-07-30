import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

export default defineConfig({
  plugins: [react()],
  // moonspace-ui ships raw TypeScript with explicit `.ts` specifiers and has no
  // build step, so it must be transpiled from source rather than consumed as a
  // built package.
  optimizeDeps: { exclude: ['moonspace-ui', 'agent-share-wasm-client'] },
  server: { fs: { allow: ['..', '../../../personal/moonspace-ui'] } },
})
