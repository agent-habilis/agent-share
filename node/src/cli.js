#!/usr/bin/env node
/**
 * `npx agent-share <🐝…> [dir]` — receive a shared folder.
 *
 * Receive only. Serving needs a filesystem scan whose sort order *is* the READ
 * index (`src/mount/scan.rs`), so a JS reimplementation that ordered
 * differently would silently corrupt every read; producing stays on the native
 * binary, which also gets you the NFS mount.
 *
 * This writes real files rather than mounting: NFS is a native-only path and
 * unreachable from wasm.
 *
 * ## Why this needs a native addon
 *
 * The relay is a rendezvous only — it carries the SDP exchange and never file
 * data — so every byte arrives over a WebRTC data channel. Node has no
 * `RTCPeerConnection`, so one has to be supplied. `node-datachannel` is an
 * optional dependency for exactly that reason: when it is present this works,
 * and when it is not the failure says so instead of hanging.
 *
 * That is a real tension with the zero-install premise of `npx`, and it is a
 * consequence of the rendezvous-only rule rather than an oversight.
 */

import { mkdir, writeFile } from 'node:fs/promises'
import { createWriteStream } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { pipeline } from 'node:stream/promises'
import { Readable } from 'node:stream'

import { safeJoin } from './paths.js'

/** The protocol's per-request ceiling (`MAX_READ_LEN`). */
const CHUNK = 256 * 1024

function usage() {
  console.error('usage: npx agent-share <🐝ticket> [destination]')
  console.error()
  console.error('  Receives a shared folder into `destination` (default: ./share).')
  console.error('  Produce a share with the native binary: agent-share serve <dir>')
}

/**
 * Install a WebRTC implementation onto `globalThis`, or explain why we cannot.
 *
 * The wasm client reaches for the platform's `RTCPeerConnection`; in a browser
 * that exists, in Node it does not.
 */
async function installWebRtc() {
  const globals = /** @type {Record<string, unknown>} */ (
    /** @type {unknown} */ (globalThis)
  )
  if (typeof globals.RTCPeerConnection === 'function') return

  try {
    const dc = /** @type {Record<string, unknown>} */ (
      /** @type {unknown} */ (await import('node-datachannel/polyfill'))
    )
    for (const name of [
      'RTCPeerConnection',
      'RTCSessionDescription',
      'RTCIceCandidate',
      'RTCDataChannel',
    ]) {
      if (dc[name]) globals[name] = dc[name]
    }
  } catch {
    throw new Error(
      'no WebRTC implementation available.\n' +
        '\n' +
        'agent-share moves file data over a direct WebRTC data channel — the relay\n' +
        'only brokers the connection and never carries file bytes. Node has no\n' +
        'built-in RTCPeerConnection, so this needs a native addon:\n' +
        '\n' +
        '    npm install node-datachannel\n' +
        '\n' +
        'Or use the native binary, which needs no addon and can also mount the\n' +
        'share as a filesystem:  agent-share <ticket> <mountpoint>',
    )
  }
}

/** Load the wasm client built by `cargo task web-wasm`. */
async function loadClient() {
  const url = new URL('../../crates/agent-share-wasm-client/dist/nodejs/agent_share_wasm_client.js', import.meta.url)
  try {
    return await import(url.href)
  } catch (cause) {
    throw new Error(
      'the wasm client is missing — build it with `cargo task web-wasm`',
      { cause },
    )
  }
}

/**
 * @typedef {{ rel_path: string, size: number, mode: number, mtime: number }} ManifestFile
 * @typedef {{ rel_path: string, mode: number, mtime: number }} ManifestDir
 * @typedef {{ dirs: ManifestDir[], files: ManifestFile[] }} Manifest
 */

/**
 * @param {string} ticket
 * @param {string} destination
 */
async function receive(ticket, destination) {
  await installWebRtc()
  const wasm = await loadClient()

  process.stderr.write('connecting over WebRTC…\n')
  const client = await wasm.ShareClient.connect(ticket)
  const manifest = /** @type {Manifest} */ (await client.manifest())

  const root = resolve(destination)
  await mkdir(root, { recursive: true })

  // Directories first so a file never races its parent.
  for (const dir of manifest.dirs) {
    await mkdir(safeJoin(root, dir.rel_path), { recursive: true })
  }

  const total = manifest.files.reduce((sum, file) => sum + file.size, 0)
  let done = 0

  for (const [index, file] of manifest.files.entries()) {
    const target = safeJoin(root, file.rel_path)
    await mkdir(dirname(target), { recursive: true })

    if (file.size === 0) {
      await writeFile(target, new Uint8Array())
      continue
    }

    // Streamed, not buffered: a share can be far larger than memory, and the
    // protocol hands back at most 256 KiB per request anyway.
    let offset = 0
    const chunks = new Readable({
      async read() {
        if (offset >= file.size) {
          this.push(null)
          return
        }
        const want = Math.min(CHUNK, file.size - offset)
        const chunk = await client.read(index, BigInt(offset), want)
        // Past-EOF is a valid empty read in this protocol, so an empty chunk
        // means the file ended — not that something failed.
        if (chunk.length === 0) {
          this.push(null)
          return
        }
        offset += chunk.length
        done += chunk.length
        process.stderr.write(`\r${((done / total) * 100).toFixed(0)}%  ${file.rel_path}`)
        this.push(Buffer.from(chunk))
      },
    })
    await pipeline(chunks, createWriteStream(target))
  }

  process.stderr.write(`\rreceived ${manifest.files.length} files into ${root}\n`)
}

const [ticket, destination = './share'] = process.argv.slice(2)
if (!ticket || ticket === '-h' || ticket === '--help') {
  usage()
  process.exit(ticket ? 0 : 2)
}

receive(ticket, destination).catch((error) => {
  process.stderr.write(`\n${error.message ?? error}\n`)
  process.exit(1)
})
