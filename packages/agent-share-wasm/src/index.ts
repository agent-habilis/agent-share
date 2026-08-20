/**
 * Load the wasm-bindgen client once per page.
 *
 * Pass an HTTP URL into `__wbg_init`. The glue's default
 * `new URL("…_bg.wasm", import.meta.url)` resolves to a `file://` path under
 * Bun's HTML/dev server (the crate lives outside `packages/`), which browsers
 * refuse to fetch.
 *
 * The path is content-addressed and generated — see `scripts/wasm-asset.ts`
 * for why a fixed name was not survivable.
 *
 * The glue in `./glue/` is wasm-bindgen's browser-target output, written there
 * by `scripts/build-wasm.ts` rather than into the crate's own build directory.
 * The dev bundler only invalidates modules inside `packages/`, so glue built
 * outside it went stale across `cargo task web-wasm` and met the fresh binary as
 * `LinkError: … function import requires a callable`. Building into the package
 * puts it where the watcher already looks.
 *
 * The CommonJS half of the same build lands in `../node/`, which
 * `agent-share-node` imports as `agent-share-wasm/node`. It needs no loader:
 * that target reads its `.wasm` sibling synchronously at import time.
 */

import type * as WasmExports from './glue/agent_share_wasm_client.js'
import { WASM_PATH } from './path.ts'

export type WasmModule = typeof WasmExports


/**
 * Caching the *promise* is load-bearing, not an optimisation. `__wbg_init`
 * guards itself with `if (wasm !== undefined) return wasm`, but it assigns
 * that module-global only *after* its await — so two overlapping calls both
 * miss the guard and each build a `WebAssembly.Instance`. The two instances
 * then share one JS glue module, whose closure table and `wasm` binding now
 * refer to the second: pointers minted by the first get read against the
 * wrong linear memory, and the page dies in `FnOnce called more than once`,
 * `function signature mismatch` and `memory access out of bounds`.
 *
 * The memo also guards any concurrent callers (not only a remounting host) —
 * two overlapping dials for the same page would hit the same bug.
 */
let wasmModule: Promise<WasmModule> | null = null

export function loadWasm(): Promise<WasmModule> {
  if (!wasmModule) {
    wasmModule = import('./glue/agent_share_wasm_client.js').then(async (module) => {
      await module.default({ module_or_path: WASM_PATH })
      return module
    })
    // A failed load must not poison every later attempt.
    wasmModule.catch(() => {
      wasmModule = null
    })
  }
  return wasmModule
}
