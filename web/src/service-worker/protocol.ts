/**
 * The messages the page and the streaming worker exchange.
 *
 * One module, imported by both sides, because they are two programs in two
 * processes that have to agree byte-for-byte and cannot be typechecked against
 * each other any other way.
 */

import type { StreamEntry } from '../lib/stream/range.ts'

/** Where a registered file is served from. See the note on the id in `sw`. */
export const STREAM_PREFIX = '/service-worker'

/** Page → worker: serve this file under `id` until told otherwise. */
export interface RegisterMessage {
  readonly type: 'register'
  readonly id: string
  readonly entry: StreamEntry
}

/** Page → worker: forget `id`. Sent when a preview closes. */
export interface ReleaseMessage {
  readonly type: 'release'
  readonly id: string
}

export type PageMessage = RegisterMessage | ReleaseMessage

/**
 * Worker → every window: whoever owns `id`, register it again.
 *
 * The browser kills an idle worker — measured here at well under a minute — and
 * a restarted one comes back with an empty registry and a dead `MessagePort`.
 * Without this, the first seek after a pause answers 404 and the video breaks;
 * with it, the page re-registers on demand, so the worker's map is a cache and
 * the page stays the source of truth.
 */
export interface WhoOwnsMessage {
  readonly type: 'whoOwns'
  readonly id: string
}

/** Worker → page, over the registration's port: fetch these bytes. */
export interface ReadMessage {
  readonly type: 'read'
  readonly reqId: number
  readonly index: number
  readonly offset: number
  readonly len: number
}

/**
 * Worker → page: stop issuing reads for `reqId`.
 *
 * A media element reopens its range request on every seek, so this is what
 * keeps an abandoned scrub from leaving a read loop running against the peer.
 */
export interface CancelMessage {
  readonly type: 'cancel'
  readonly reqId: number
}

export type WorkerMessage = ReadMessage | CancelMessage

/** Page → worker, answering a `read`. `bytes` is transferred, not copied. */
export interface BytesMessage {
  readonly type: 'bytes'
  readonly reqId: number
  readonly bytes: ArrayBuffer
}

/** Page → worker: this read failed, and the stream should error. */
export interface ErrorMessage {
  readonly type: 'error'
  readonly reqId: number
  readonly message: string
}

export type ReadReply = BytesMessage | ErrorMessage
