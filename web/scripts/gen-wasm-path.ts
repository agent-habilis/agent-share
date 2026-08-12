/**
 * Write `src/wasm-path.ts` and stop.
 *
 * `dev.ts`, `build.ts` and `start.ts` each generate it on their own way past.
 * The scripts that only *read* the tree — `typecheck`, `test` — have no such
 * step, and the file is not in git (it holds the hash of whatever this machine
 * last built), so on a fresh clone they would fail on a missing import.
 */

import { wasmAsset, writeWasmPath } from './wasm-asset.ts'

await writeWasmPath(await wasmAsset())
