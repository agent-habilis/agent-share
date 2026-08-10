/**
 * The page's half of the streaming service worker.
 *
 * Registers the worker, hands it one file at a time, and answers the reads it
 * asks for. The worker parses `Range`; this side owns the bytes, because it is
 * the only process holding both the peer connection and the chunk store — see
 * the header of `src/sw/index.ts`.
 *
 * **Every entry point returns `null` rather than throwing when the worker is
 * unavailable.** No service worker on this browser, an insecure context, Safari
 * private browsing, a registration the user cleared — all of them are ordinary,
 * and all of them mean the caller falls back to the whole-file Blob path it used
 * before. A preview that fails because streaming was unavailable would be a
 * worse product than one that buffers.
 */

import {
  STREAM_PREFIX,
  type ReadReply,
  type WhoOwnsMessage,
  type WorkerMessage,
} from '../../sw/protocol.ts'
import type { StreamEntry } from './range.ts'

/** What a caller needs to read one file. */
export interface StreamReader {
  read(index: number, offset: bigint, len: number): Promise<Uint8Array>
  readonly source_is_origin?: boolean
}

/** A registered file: its URL, and the way to take it back down. */
export interface Stream {
  readonly url: string
  release(): void
}

/** Registration is attempted once per page; the promise is the memo. */
let ready: Promise<ServiceWorkerRegistration | null> | null = null

/**
 * Files this page has published, by id — **the durable copy**.
 *
 * The worker's own registry is a cache: the browser kills an idle worker (well
 * under a minute, measured) and it comes back empty, which used to answer the
 * first seek after a pause with a 404 and break the element. When that happens
 * the worker asks the windows who owns the id, and this is what answers.
 *
 * Keyed by id rather than held by the caller, because the asking arrives on the
 * page's own `serviceWorker` channel, long after `openStream` returned.
 */
const published = new Map<string, () => void>()

/** Wired once, for as long as this page lives. */
let listening = false

function listenForRecovery(): void {
  if (listening || !('serviceWorker' in navigator)) return
  listening = true
  navigator.serviceWorker.addEventListener('message', (event: MessageEvent) => {
    const message = event.data as WhoOwnsMessage | undefined
    if (message?.type !== 'whoOwns') return
    // Only the page that still holds it answers; everyone else stays quiet,
    // which is what keeps a second tab from claiming another's stream.
    published.get(message.id)?.()
  })
}

/**
 * The controlling worker, registering it if this is the first ask.
 *
 * Caching the *promise* rather than the result is what keeps two previews
 * opened at once from racing two registrations — the same reason `loadWasm`
 * memoises its own.
 */
function worker(): Promise<ServiceWorkerRegistration | null> {
  if (ready) return ready
  ready = (async () => {
    if (!('serviceWorker' in navigator)) return null
    try {
      // Root scope, which is what the script's own path buys: a worker's
      // default scope is its directory, and only a root-served script can
      // control `/__stream/…`.
      const registration = await navigator.serviceWorker.register('/sw.js', { scope: '/' })
      await navigator.serviceWorker.ready
      return registration
    } catch {
      return null
    }
  })()
  return ready
}

/** Ids are per-registration and opaque: never the ticket, never the path. */
function mintId(): string {
  const bytes = new Uint8Array(16)
  crypto.getRandomValues(bytes)
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0')).join('')
}

/**
 * Publish `entry` at a URL the worker will answer with ranges.
 *
 * `null` means streaming is unavailable and the caller should buffer instead.
 * `name` is cosmetic — it is what a download shelf displays.
 */
export async function openStream(
  reader: StreamReader,
  entry: Omit<StreamEntry, 'sourceIsOrigin'>,
  name: string,
): Promise<Stream | null> {
  const registration = await worker()
  const active = registration?.active ?? navigator.serviceWorker?.controller
  if (!active) return null

  const id = mintId()
  let channel = new MessageChannel()

  // Forwarded, never defaulted. A seeder serves a frozen snapshot, so a short
  // read from one is truncation rather than EOF — and the worker cannot know
  // which it is talking to.
  const full: StreamEntry = { ...entry, sourceIsOrigin: reader.source_is_origin !== false }

  const cancelled = new Set<number>()
  channel.port1.onmessage = (event: MessageEvent) => {
    const message = event.data as WorkerMessage | undefined
    if (!message) return
    if (message.type === 'cancel') {
      // The worker sends a bare cancel for the response being torn down. There
      // is no way to abort the read already in flight; what this does is stop
      // the next one from being issued.
      cancelled.add(message.reqId)
      return
    }
    void answer(message.reqId, message.index, message.offset, message.len)
  }

  async function answer(reqId: number, index: number, offset: number, len: number) {
    const reply = (message: ReadReply, transfer: Transferable[] = []) => {
      channel.port1.postMessage(message, transfer)
    }
    try {
      const bytes = await reader.read(index, BigInt(offset), len)
      // Copied out of the wasm heap before transfer: a view onto wasm memory
      // cannot be detached, and the copy is what makes the hand-off zero-cost
      // on the receiving side.
      const buffer = bytes.slice().buffer
      reply({ type: 'bytes', reqId, bytes: buffer }, [buffer])
    } catch (error) {
      reply({ type: 'error', reqId, message: error instanceof Error ? error.message : String(error) })
    }
  }

  active.postMessage({ type: 'register', id, entry: full }, [channel.port2])

  // The port cannot outlive a worker restart — it is torn down with the process
  // on the other end — so recovery mints a fresh channel rather than resending
  // the old one.
  listenForRecovery()
  published.set(id, () => {
    const revived = new MessageChannel()
    revived.port1.onmessage = channel.port1.onmessage
    channel = revived
    const current = navigator.serviceWorker?.controller ?? active
    current.postMessage({ type: 'register', id, entry: full }, [revived.port2])
  })

  return {
    url: `${STREAM_PREFIX}/${id}/${encodeURIComponent(name)}`,
    release() {
      published.delete(id)
      const current = navigator.serviceWorker?.controller ?? active
      current.postMessage({ type: 'release', id })
      channel.port1.close()
    },
  }
}
