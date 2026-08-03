import { defineConfig, type Plugin } from 'vite'

/**
 * Serve the bench/mesh lab at `/lab`, not `/lab.html`.
 *
 * The page itself is `lab/index.html`, so `/lab/` already resolves on any
 * static host that does directory indexes — but the bare `/lab` never reaches
 * that lookup in dev: Vite's SPA fallback claims every extensionless path and
 * hands back the app's `index.html`, so `/lab` silently rendered the share
 * browser instead. This rewrites it before the fallback sees it.
 */
function labRoute(): Plugin {
  return {
    name: 'agent-share-lab-route',
    configureServer(server) {
      server.middlewares.use((req, _res, next) => {
        if (req.url === '/lab' || req.url?.startsWith('/lab?')) {
          req.url = `/lab/index.html${req.url.slice('/lab'.length)}`
        }
        next()
      })
    },
  }
}

// Safari has no Explicit Resource Management, and `using` — visage-dom's whole
// resource idiom — is a syntax error there, which aborts the module rather than
// failing a feature check. Oxc only lowers it for an ES-version target: its
// browser table claims even `safari26` supports it, and Vite's own defaults
// (`esnext` in dev, `baseline-widely-available` for the build) pass it through.
// Hence es2023 on both, paired with `src/compat.ts` for the symbols the emitted
// `usingCtx` helper looks the dispose method up under.
const TARGET = 'es2023'

export default defineConfig({
  plugins: [labRoute()],
  // Vite's transform reads jsx settings from tsconfig (jsxImportSource: visage-dom).
  optimizeDeps: { exclude: ['agent-share-wasm-client'] },
  // Still needed for the wasm import from ../crates/...
  server: { fs: { allow: ['..'] } },
  oxc: { target: TARGET },
  build: { target: TARGET },
})
