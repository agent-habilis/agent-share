/**
 * The wasm binary, addressed by the hash of its contents.
 *
 * # Why the filename carries a hash
 *
 * The binary used to be served at one fixed path, and the stale copy that
 * produced is not a theoretical problem — it cost three debugging detours in
 * one session. It fails in the worst possible way: the app loads, runs, and
 * then rejects perfectly good input, because an *old* build is answering. The
 * symptom (`decode ticket: invalid ticket address json`) points at the ticket,
 * which is fine, rather than at the binary, which is not.
 *
 * Every layer had its own reason to hold the old bytes — Bun's route table
 * captures a `Bun.file` at server start, the browser caches by URL, and a
 * static host in front of `dist/` would too. Chasing each one separately is
 * endless; making the URL change whenever the content changes ends all of them
 * at once, because none of those caches can answer for a URL they have never
 * seen.
 *
 * # No stable alias, deliberately
 *
 * The old path is *not* kept as an alias. A stale reference now 404s, which is
 * loud and immediate, rather than quietly resolving to whatever was there
 * before. Given the failure this replaces, a missing file is the better bug.
 *
 * # Why the URL has a `/wasm/` prefix
 *
 * "404s" is a promise the server has to be able to keep, and at the URL root it
 * could not. The app's SPA catch-all matches every unclaimed path, so a request
 * for a hash the server does not have was answered with `index.html` — which
 * the browser dutifully fed to `WebAssembly.instantiate`, producing `expected
 * magic word 00 61 73 6d, found 3c 21 64 6f` (`3c 21 64 6f` is `<!do`). The
 * prefix gives the binary a route of its own that outranks the catch-all, so a
 * miss can answer for itself.
 */

/** The `web/` directory, so paths below read as they do from a shell there. */
const WEB_ROOT = new URL('../', import.meta.url)

/** Where `cargo task web-wasm` leaves the binary, relative to `web/`. */
const WASM_SOURCE =
  '../crates/agent-share-wasm-client/dist/web/agent_share_wasm_client_bg.wasm'

/**
 * The binary, resolved. Exported already-resolved rather than as a relative
 * string: the string only means anything against `web/`, and a caller that
 * re-resolves it against its own module URL silently addresses a path that
 * never exists — which reads as "wasm missing" rather than as a bad base.
 */
export const WASM_FILE = new URL(WASM_SOURCE, WEB_ROOT)

/** Module holding the generated path, imported by `src/wasm/index.ts`. */
const GENERATED = new URL('src/wasm/path.ts', WEB_ROOT)

/** URL directory the binary is served from. See the header. */
export const WASM_DIR = '/wasm'

export interface WasmAsset {
  bytes: Uint8Array
  /** Short content hash. */
  hash: string
  /** Filename including the hash. */
  name: string
  /** Absolute URL path the app fetches. */
  path: string
  /** Precompressed bodies, when a caller attached them — the binary is ~7 MB
   * raw and ~2 MB compressed, and that difference sits on the connect path. */
  br?: Uint8Array
  gz?: Uint8Array
}

/**
 * Read the wasm and derive its content-addressed name, or `null` if it is not
 * readable right now.
 *
 * The nullable case is not just missing-file pedantry: `cargo task web-wasm`
 * replaces this binary while the dev server is watching it, and for part of
 * that window the path does not resolve. A watcher built on {@link wasmAsset}
 * would take the whole server down on every rebuild.
 */
export async function tryWasmAsset(): Promise<WasmAsset | null> {
  const file = Bun.file(WASM_FILE)
  let bytes: Uint8Array
  try {
    if (!(await file.exists())) return null
    bytes = new Uint8Array(await file.arrayBuffer())
  } catch {
    return null
  }
  // SHA-256 truncated to 12 hex. Not a security boundary — the binary is
  // served from our own origin — just enough that two different builds cannot
  // collide onto one URL.
  const hash = new Bun.CryptoHasher('sha256')
    .update(bytes)
    .digest('hex')
    .slice(0, 12)
  const name = `agent_share_wasm_client_bg.${hash}.wasm`
  return { bytes, hash, name, path: `${WASM_DIR}/${name}` }
}

/**
 * Read the wasm and derive its content-addressed name.
 *
 * Exits rather than throwing when the binary is missing: every caller is a
 * top-level script, and the actionable part is the instruction, not a stack.
 */
export async function wasmAsset(): Promise<WasmAsset> {
  const asset = await tryWasmAsset()
  if (!asset) {
    console.error('wasm missing — run `cargo task web-wasm` first')
    process.exit(1)
  }
  return asset
}

/** Attach a gzip body — cheap enough to run on every dev rebuild. */
export function withGzip(asset: WasmAsset): WasmAsset {
  asset.gz = Bun.gzipSync(asset.bytes, { level: 6 })
  return asset
}

/**
 * Brotli at build quality. Seconds, not milliseconds, on a 7 MB binary —
 * which is why only `build.ts` calls it, once, and the dev server settles for
 * gzip.
 */
export async function brotli(bytes: Uint8Array): Promise<Uint8Array> {
  const { brotliCompressSync, constants } = await import('node:zlib')
  return new Uint8Array(
    brotliCompressSync(bytes, {
      params: {
        [constants.BROTLI_PARAM_QUALITY]: 10,
        [constants.BROTLI_PARAM_SIZE_HINT]: bytes.length,
      },
    }),
  )
}

/**
 * The binary as an HTTP response, negotiated against `accept-encoding`.
 *
 * Shared so the headers are stated once. The cache directive is only safe
 * because of the hash in the URL, and that pairing is easier to keep true in
 * one place than in every server that serves the file. `content-type` stays
 * `application/wasm` whatever the encoding, so `instantiateStreaming` still
 * engages in the glue.
 */
export function wasmResponse(
  asset: WasmAsset,
  acceptEncoding?: string | null,
): Response {
  const headers: Record<string, string> = {
    'content-type': 'application/wasm',
    // Safe to cache hard: the URL changes when the bytes do.
    'cache-control': 'public, max-age=31536000, immutable',
    vary: 'accept-encoding',
  }
  const accepted = acceptEncoding ?? ''
  let body = asset.bytes
  if (asset.br && /\bbr\b/.test(accepted)) {
    body = asset.br
    headers['content-encoding'] = 'br'
  } else if (asset.gz && /\bgzip\b/.test(accepted)) {
    body = asset.gz
    headers['content-encoding'] = 'gzip'
  }
  return new Response(body as unknown as BodyInit, { headers })
}

/** The glue files wasm-bindgen leaves beside the binary. The `.d.ts` rides
 * along so the type-only import in `src/wasm/index.ts` resolves against the same
 * mirror the runtime import uses. */
const GLUE_SOURCES = ['agent_share_wasm_client.js', 'agent_share_wasm_client.d.ts'] as const

/** Where the glue lands inside `src/wasm/` — generated, gitignored. */
const GLUE_DIR = new URL('src/wasm/glue/', WEB_ROOT)

/** The directory the glue is mirrored from. */
const DIST_DIR = new URL('./', WASM_FILE)

/**
 * Mirror the JS glue into `src/`, if it changed. Returns whether it wrote.
 *
 * The binary heals itself through the content-addressed URL, but the glue
 * used to be imported straight out of `dist/` — which sits outside `web/`,
 * where the dev bundler's watcher never looks. `cargo task web-wasm`
 * mid-session therefore produced a page whose *wasm* was fresh and whose
 * *glue* was whatever the bundler cached at server start; the mismatch
 * surfaces as `LinkError: … function import requires a callable` naming a
 * binding only one side knows about. Mirroring into `src/` puts the glue
 * where the watcher already is, the same move `writeWasmPath` makes for the
 * path. Content-guarded for the same reason: an unconditional write would
 * rebundle on every start.
 */
export async function syncGlue(): Promise<boolean> {
  let wrote = false
  for (const name of GLUE_SOURCES) {
    const source = Bun.file(new URL(name, DIST_DIR))
    let text: string
    try {
      if (!(await source.exists())) continue
      text = await source.text()
    } catch {
      // Mid-rebuild, like the binary: wasm-bindgen replaces these files in
      // stages, and a half-written glue must not take the mirror down.
      continue
    }
    const target = Bun.file(new URL(name, GLUE_DIR))
    if ((await target.exists()) && (await target.text()) === text) continue
    await Bun.write(new URL(name, GLUE_DIR), text)
    wrote = true
  }
  return wrote
}

/**
 * Write the generated path module, if it changed.
 *
 * Guarded on the contents because `bun --hot` watches `src/`: writing the same
 * text every start would rebuild the app on every start, and writing it in a
 * loop would never settle.
 */
export async function writeWasmPath(asset: WasmAsset): Promise<void> {
  const source = `/**
 * GENERATED by \`scripts/wasm-asset.ts\` — do not edit.
 *
 * The wasm binary's content-addressed path. Regenerated by \`scripts/dev.ts\`,
 * \`scripts/start.ts\` and \`scripts/build.ts\` before they serve or bundle,
 * so a stale binary cannot be served under a URL the browser has already
 * cached.
 */

export const WASM_PATH = '${asset.path}'
`
  const existing = Bun.file(GENERATED)
  if ((await existing.exists()) && (await existing.text()) === source) return
  await Bun.write(GENERATED, source)
}
