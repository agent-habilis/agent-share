import { defineConfig } from 'vite'

export default defineConfig({
  // Vite's transform reads jsx settings from tsconfig (jsxImportSource: visage-dom).
  optimizeDeps: { exclude: ['agent-share-wasm-client'] },
  // Still needed for the wasm import from ../crates/...
  server: { fs: { allow: ['..'] } },
})
