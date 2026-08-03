/**
 * Load the wasm-bindgen client once per page.
 *
 * Pass an HTTP URL into `__wbg_init`. The glue's default
 * `new URL("…_bg.wasm", import.meta.url)` resolves to a `file://` path under
 * Bun's HTML/dev server (the crate lives outside `web/`), which browsers
 * refuse to fetch.
 */

import type * as WasmExports from '../../crates/agent-share-wasm-client/dist/web/agent_share_wasm_client.js'

export type WasmModule = typeof WasmExports

/** Served by `dev.ts` / `preview.ts`, and copied into `dist/` by `build.ts`. */
const WASM_PATH = '/agent_share_wasm_client_bg.wasm'

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
    wasmModule = import(
      '../../crates/agent-share-wasm-client/dist/web/agent_share_wasm_client.js'
    ).then(async (module) => {
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
