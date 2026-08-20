/**
 * Start an in-browser share from files the user picked.
 *
 * Two ways in, one producer. Chromium has `showDirectoryPicker()`, which yields
 * a directory handle: the tree is walked here (async iterators are awkward from
 * wasm) and a chained-timeout rescan keeps the share **live**. Safari and
 * Firefox have no such picker, so they pick through `<input type="file">` and
 * the share is a **snapshot** — the same bytes, read the same lazy way, but
 * pinned to what was picked, because rescanning would need a fresh gesture.
 *
 * Either way the wasm `ShareProducer` binds the endpoint and serves
 * READ/MANIFEST/WATCH.
 */

import { buildPeerCard } from './peer-card/index.ts'
import { snapshotListing, type SnapshotListing } from './snapshot/index.ts'
import { loadWasm } from 'agent-share-wasm'

export interface ShareProducer {
  readonly ticket: string
  readonly transport: string
  readonly files: number
  readonly bytes: number
  /**
   * Whether this share is behind a password — so the UI can say the ticket is
   * not on its own enough, beside the link it offers to copy.
   */
  readonly passwordProtected: boolean
  /**
   * Whether the share follows the folder. False for a snapshot, where edits on
   * disk never reach a peer and empty folders were never in the listing. The UI
   * has to say so: a sender who does not know will read the peer's read error
   * as a bug in the transport.
   */
  readonly live: boolean
  /** Picked files left out because their path could escape the share root. */
  readonly skipped: number
  /** Picked files served under a suffixed name after a path collision. */
  readonly renamed: number
  stop(): Promise<void>
}

/**
 * Whether this browser can serve a share that follows the folder.
 *
 * Not a gate on sharing at all — every browser has a file input, and
 * [`startProducer`] serves a snapshot from one. This only decides which picker
 * to open and whether the result gets a rescan loop.
 */
export function canProduceLive(): boolean {
  return typeof window.showDirectoryPicker === 'function'
}

/** Ask the user for a folder to share. AbortError propagates for cancel. */
export async function pickShareRoot(): Promise<FileSystemDirectoryHandle> {
  const picker = window.showDirectoryPicker
  if (!picker) {
    throw new Error('This browser has no directory picker')
  }
  return picker({ mode: 'read' })
}

/**
 * Where a share's bytes come from.
 *
 * Tagged `from` rather than `kind` because `FileSystemHandle` already has a
 * `kind`, and a union that shares a discriminant with one of its own members'
 * payloads cannot be narrowed.
 */
export type ShareSource =
  | { from: 'directory'; root: FileSystemDirectoryHandle }
  | { from: 'snapshot'; listing: SnapshotListing<File> }

/** A live source from a picked directory. */
export function directorySource(root: FileSystemDirectoryHandle): ShareSource {
  return { from: 'directory', root }
}

/** A snapshot source from what a file input handed back. */
export function snapshotSource(picked: readonly File[]): ShareSource {
  return { from: 'snapshot', listing: snapshotListing(picked) }
}

function safeComponent(name: string): boolean {
  return name !== '' && name !== '.' && name !== '..' && !name.includes('\0')
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

const POLL_MS = 2000

interface Listing {
  dirs: string[]
  files: {
    rel_path: string
    size: number
    mtime: number
    source: FileSystemFileHandle | File
  }[]
}

async function scanDirectory(root: FileSystemDirectoryHandle): Promise<Listing> {
  const dirs: string[] = []
  const files: Listing['files'] = []

  async function walk(dir: FileSystemDirectoryHandle, prefix: string): Promise<void> {
    for await (const [name, handle] of dir.entries()) {
      if (!safeComponent(name)) continue
      const path = prefix ? `${prefix}/${name}` : name
      if (handle.kind === 'directory') {
        dirs.push(path)
        await walk(handle as FileSystemDirectoryHandle, path)
      } else {
        const fileHandle = handle as FileSystemFileHandle
        const file = await fileHandle.getFile()
        files.push({
          rel_path: path,
          size: file.size,
          mtime: Math.floor(file.lastModified / 1000),
          source: fileHandle,
        })
      }
    }
  }

  await walk(root, '')
  return { dirs, files }
}

/**
 * Bind a producer on `source` and return a handle that keeps serving until stop.
 *
 * Pass a `password` to protect the share: the ticket then addresses it without
 * opening it, so the link is safe to post somewhere the password is not. Costs
 * ~100 ms of Argon2id on the main thread, once, here — the same price every
 * viewer pays when they open it.
 */
export async function startProducer(
  source: ShareSource,
  password?: string,
): Promise<ShareProducer> {
  const live = source.from === 'directory'
  const listing = source.from === 'directory' ? await scanDirectory(source.root) : source.listing

  const wasm = await loadWasm()
  const producer = await wasm.ShareProducer.start(
    listing,
    buildPeerCard({ role: 'producer', transport: 'webrtc' }),
    password,
  )

  let stopped = false
  if (source.from === 'directory') {
    const { root } = source
    ;(async () => {
      while (!stopped) {
        await sleep(POLL_MS)
        try {
          const next = await scanDirectory(root)
          if (!stopped) producer.update(next)
        } catch (error) {
          console.warn('[share] rescan failed; serving previous tree', error)
        }
      }
    })()
  }

  return {
    ticket: producer.ticket,
    transport: producer.transport,
    files: producer.files,
    bytes: Number(producer.bytes),
    // Read back from what was actually asked for, not re-derived from the
    // ticket: this is the producer's own knowledge of its own share.
    passwordProtected: password !== undefined,
    live,
    skipped: source.from === 'snapshot' ? source.listing.skipped : 0,
    renamed: source.from === 'snapshot' ? source.listing.renamed : 0,
    stop: async () => {
      stopped = true
      await producer.stop()
    },
  }
}
