/**
 * The streaming service worker: an HTTP shim in front of the page's peer
 * connection.
 *
 * # Why a worker at all
 *
 * A `<video src>` pointed at a Blob URL cannot seek, because every byte has to
 * exist before the URL does. Pointed at an origin that answers `Range` with 206,
 * the element does its own seeking, and starts playing before the file is
 * complete. This is that origin.
 *
 * # Why it holds no bytes of its own
 *
 * The browser data plane is `RTCPeerConnection`, which does not exist in
 * `ServiceWorkerGlobalScope` — so a worker can never hold the connection. It
 * *could* host a wasm instance and read the chunk store directly (`IdbStore`
 * reaches `indexedDB` through `js_sys::global()`, not `window`, so it works in a
 * worker) and that is not ruled out on capability. It is not done because the
 * page is the only process holding **both** halves: keeping the store/network
 * decision there means one policy rather than two to keep in sync, no second
 * ~7 MB wasm instantiation in a process the browser kills at ~30 s idle, and no
 * two-reader coherence question against a store the page writes as chunks land.
 *
 * So: the worker parses `Range`, and the page fetches bytes.
 *
 * # The id in the URL is opaque on purpose
 *
 * `/service-worker/<id>/<name>` carries a per-registration key, never the ticket and
 * never the manifest path. The ticket is a bearer capability for the whole
 * share; putting it in a URL would spread it into worker scope, into anything
 * that logs a request, and into the browser's download shelf. `<name>` is
 * cosmetic — it is what the shelf shows the user.
 *
 * Replies ride the `MessagePort` the registration arrived on, so the worker
 * never has to find a client by id and an answer cannot land in the wrong tab —
 * which matters here, because several tabs of one share is the ordinary case.
 */

import { respond, type StreamEntry } from 'agent-share-core/stream/range'
import { STREAM_PREFIX, type PageMessage, type ReadReply } from 'agent-share-core/stream/protocol'

/*
 * The worker's own global, declared here rather than pulled in as a lib.
 *
 * `lib: ["dom", …]` and `lib: ["webworker"]` cannot both be on — they declare
 * the same names with different types, and this package typechecks as one
 * project against `dom`. Adding a second tsconfig for one file costs more
 * than the dozen lines below, and these are only the members this worker
 * actually touches, so anything it grows into fails to compile rather than
 * silently resolving to `any`.
 */
interface ExtendableEvent extends Event {
  waitUntil(promise: Promise<unknown>): void
}
interface FetchEvent extends ExtendableEvent {
  readonly request: Request
  respondWith(response: Response | Promise<Response>): void
}
interface ExtendableMessageEvent extends ExtendableEvent {
  readonly data: unknown
  readonly ports: readonly MessagePort[]
}
interface WindowClient {
  postMessage(message: unknown): void
}
interface ServiceWorkerScope {
  readonly location: Location
  readonly clients: {
    claim(): Promise<void>
    matchAll(options?: { type?: 'window' }): Promise<WindowClient[]>
  }
  skipWaiting(): Promise<void>
  addEventListener(type: 'install' | 'activate', listener: (event: ExtendableEvent) => void): void
  addEventListener(type: 'fetch', listener: (event: FetchEvent) => void): void
  addEventListener(type: 'message', listener: (event: ExtendableMessageEvent) => void): void
}
declare const self: ServiceWorkerScope

/** What the page registered, by id. */
interface Registration {
  readonly entry: StreamEntry
  readonly port: MessagePort
  /** Reads waiting on this port, by request id. */
  readonly waiting: Map<number, (reply: ReadReply) => void>
}

const registered = new Map<string, Registration>()
let nextReqId = 1

/**
 * Ids being re-registered right now, and who is waiting on each.
 *
 * See [`recover`]. Keyed so several ranges racing for one evicted id all wake on
 * the single re-registration rather than each asking for their own.
 */
const recovering = new Map<string, Array<(held: Registration | null) => void>>()

/** How long a recovery waits before giving up and answering 404. */
const RECOVER_MS = 3000

/**
 * Get `id` back after the browser threw this worker away.
 *
 * A worker is killed when idle — under a minute, in practice — and comes back
 * with an empty registry and a dead port, so the first seek after a pause used
 * to answer 404 and break the element. The durable copy lives in the page, so
 * ask every window to register it again.
 *
 * Bounded rather than open-ended: if no window owns it, the file really is gone
 * (the preview closed, the tab navigated), and a `fetch` that never settles
 * would hang the element instead of failing it.
 */
function recover(id: string): Promise<Registration | null> {
  const waiting = recovering.get(id)
  if (waiting) {
    return new Promise((resolve) => waiting.push(resolve))
  }
  return new Promise((resolve) => {
    recovering.set(id, [resolve])
    const settle = (held: Registration | null) => {
      const all = recovering.get(id)
      if (!all) return
      recovering.delete(id)
      for (const wake of all) wake(held)
    }
    setTimeout(() => settle(registered.get(id) ?? null), RECOVER_MS)
    void self.clients.matchAll({ type: 'window' }).then((windows) => {
      for (const window of windows) window.postMessage({ type: 'whoOwns', id })
    })
  })
}

/**
 * Take over as soon as possible.
 *
 * Without both of these the first load after an update answers from the old
 * worker — or from no worker, which on the dev server means the SPA catch-all
 * returns `index.html` to a media element and the failure surfaces as a codec
 * error naming the wrong problem.
 */
self.addEventListener('install', () => {
  void self.skipWaiting()
})
self.addEventListener('activate', (event) => {
  event.waitUntil(self.clients.claim())
})

self.addEventListener('message', (event) => {
  const data = event.data as PageMessage | undefined
  if (!data) return

  if (data.type === 'register') {
    const port = event.ports[0]
    if (!port) return
    const waiting = new Map<number, (reply: ReadReply) => void>()
    port.onmessage = (reply: MessageEvent) => {
      const answer = reply.data as ReadReply | undefined
      if (!answer) return
      const settle = waiting.get(answer.reqId)
      if (!settle) return
      waiting.delete(answer.reqId)
      settle(answer)
    }
    port.start()
    const held: Registration = { entry: data.entry, port, waiting }
    registered.set(data.id, held)
    // A recovery in flight is waiting on exactly this.
    const woken = recovering.get(data.id)
    if (woken) {
      recovering.delete(data.id)
      for (const wake of woken) wake(held)
    }
    return
  }

  if (data.type === 'release') {
    const held = registered.get(data.id)
    if (!held) return
    // Anything still waiting is answered rather than left pending: a promise
    // that never settles would hold its `ReadableStream` — and the response the
    // browser is streaming — open forever.
    for (const settle of held.waiting.values()) {
      settle({ type: 'error', reqId: 0, message: 'the page released this stream' })
    }
    held.waiting.clear()
    held.port.close()
    registered.delete(data.id)
  }
})

/** Ask the page for bytes, and wait for the reply on the registration's port. */
function readVia(held: Registration, index: number) {
  return (offset: number, len: number) =>
    new Promise<Uint8Array>((resolve, reject) => {
      const reqId = nextReqId++
      held.waiting.set(reqId, (reply) => {
        if (reply.type === 'error') {
          reject(new Error(reply.message))
          return
        }
        resolve(new Uint8Array(reply.bytes))
      })
      held.port.postMessage({ type: 'read', reqId, index, offset, len })
    })
}

/**
 * The id and name in a stream URL, or `null` for anything else.
 *
 * Matched against this worker's own origin: a cross-origin request that happens
 * to carry the same path is not ours to answer.
 */
function streamPath(url: URL): { id: string } | null {
  if (url.origin !== self.location.origin) return null
  if (!url.pathname.startsWith(`${STREAM_PREFIX}/`)) return null
  const [, , id] = url.pathname.split('/')
  return id ? { id } : null
}

self.addEventListener('fetch', (event) => {
  const url = new URL(event.request.url)
  const target = streamPath(url)
  if (!target) return

  const known = registered.get(target.id)
  event.respondWith(
    (async () => {
      // An unknown id is usually not an unknown *file*: this worker was evicted
      // between the first byte and this seek, and came back empty. Ask the
      // windows to register it again before concluding it is gone.
      const held = known ?? (await recover(target.id))
      if (!held) {
        // Genuinely nothing to serve. 404 rather than falling through to the
        // network, where the SPA catch-all would hand back `index.html` and the
        // element would report a decode failure instead.
        return new Response(null, { status: 404 })
      }
      // Cancellation is what keeps a scrub from leaking: the element drops and
      // reopens its range request on every seek, and the read already in flight
      // cannot be aborted — so the page is told to stop issuing more.
      const cancel = () => held.port.postMessage({ type: 'cancel', reqId: 0 })
      return respond(event.request, held.entry, readVia(held, held.entry.index), cancel)
    })(),
  )
})
