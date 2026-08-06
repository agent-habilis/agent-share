/**
 * Start an in-browser share from a picked directory.
 *
 * The directory is walked here (async iterators are awkward from wasm); the
 * wasm `ShareProducer` binds the endpoint and serves READ/MANIFEST/WATCH.
 * A chained-timeout rescan keeps the share live while the producer runs.
 */

import { buildPeerCard } from './peerCard/index.ts'
import { loadWasm } from './wasm.ts'

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
  stop(): Promise<void>
}

export function canProduce(): boolean {
  return typeof window.showDirectoryPicker === 'function'
}

/** Ask the user for a folder to share. AbortError propagates for cancel. */
export async function pickShareRoot(): Promise<FileSystemDirectoryHandle> {
  const picker = window.showDirectoryPicker
  if (!picker) {
    throw new Error('This browser cannot share folders')
  }
  return picker({ mode: 'read' })
}

function safeComponent(name: string): boolean {
  return name !== '' && name !== '.' && name !== '..' && !name.includes('\0')
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

const POLL_MS = 2000

async function scanDirectory(root: FileSystemDirectoryHandle): Promise<{
  dirs: string[]
  files: {
    rel_path: string
    size: number
    mtime: number
    handle: FileSystemFileHandle
  }[]
}> {
  const dirs: string[] = []
  const files: {
    rel_path: string
    size: number
    mtime: number
    handle: FileSystemFileHandle
  }[] = []

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
          handle: fileHandle,
        })
      }
    }
  }

  await walk(root, '')
  return { dirs, files }
}

/**
 * Bind a producer on `root` and return a handle that keeps serving until stop.
 *
 * Pass a `password` to protect the share: the ticket then addresses it without
 * opening it, so the link is safe to post somewhere the password is not. Costs
 * ~100 ms of Argon2id on the main thread, once, here — the same price every
 * viewer pays when they open it.
 */
export async function startProducer(
  root: FileSystemDirectoryHandle,
  password?: string,
): Promise<ShareProducer> {
  const listing = await scanDirectory(root)
  const wasm = await loadWasm()
  const producer = await wasm.ShareProducer.start(
    listing,
    buildPeerCard({ role: 'producer', transport: 'webrtc' }),
    password,
  )

  let stopped = false
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

  return {
    ticket: producer.ticket,
    transport: producer.transport,
    files: producer.files,
    bytes: Number(producer.bytes),
    // Read back from what was actually asked for, not re-derived from the
    // ticket: this is the producer's own knowledge of its own share.
    passwordProtected: password !== undefined,
    stop: async () => {
      stopped = true
      await producer.stop()
    },
  }
}
