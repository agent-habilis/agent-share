/**
 * Start an in-browser share from a picked directory.
 *
 * The directory is walked here (async iterators are awkward from wasm); the
 * wasm `ShareProducer` binds the endpoint and serves READ/MANIFEST/WATCH.
 * A chained-timeout rescan keeps the share live while the producer runs.
 */

import { buildPeerCard } from './peerCard.ts'

export interface ShareProducer {
  readonly ticket: string
  readonly transport: string
  readonly files: number
  readonly bytes: number
  stop(): Promise<void>
}

function importWasm() {
  return import(
    '../../crates/agent-share-wasm-client/dist/web/agent_share_wasm_client.js'
  )
}

type WasmModule = Awaited<ReturnType<typeof importWasm>>

let wasmModule: Promise<WasmModule> | null = null

function loadWasm(): Promise<WasmModule> {
  if (!wasmModule) {
    wasmModule = importWasm().then(async (module) => {
      await module.default()
      return module
    })
    // A failed load must not poison every later attempt.
    wasmModule.catch(() => {
      wasmModule = null
    })
  }
  return wasmModule
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

/** Bind a producer on `root` and return a handle that keeps serving until stop. */
export async function startProducer(root: FileSystemDirectoryHandle): Promise<ShareProducer> {
  const listing = await scanDirectory(root)
  const wasm = await loadWasm()
  const producer = await wasm.ShareProducer.start(
    listing,
    buildPeerCard({ role: 'producer', transport: 'webrtc' }),
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
    stop: async () => {
      stopped = true
      await producer.stop()
    },
  }
}
