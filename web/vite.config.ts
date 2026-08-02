import { defineConfig } from 'vite'

// Safari has no Explicit Resource Management, and `using` — visage-dom's whole
// resource idiom — is a syntax error there, which aborts the module rather than
// failing a feature check. Oxc only lowers it for an ES-version target: its
// browser table claims even `safari26` supports it, and Vite's own defaults
// (`esnext` in dev, `baseline-widely-available` for the build) pass it through.
// Hence es2023 on both, paired with `src/compat.ts` for the symbols the emitted
// `usingCtx` helper looks the dispose method up under.
const TARGET = 'es2023'

export default defineConfig({
  // Vite's transform reads jsx settings from tsconfig (jsxImportSource: visage-dom).
  optimizeDeps: { exclude: ['agent-share-wasm-client'] },
  // Still needed for the wasm import from ../crates/...
  server: { fs: { allow: ['..'] } },
  oxc: { target: TARGET },
  build: { target: TARGET },
})
