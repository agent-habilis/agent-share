#!/usr/bin/env node
/**
 * `npx agent-share <ticket> [dir] [--password <pw>]` — receive a shared folder.
 * `npx agent-share bench --transport webrtc` — synthetic OP_BENCH producer.
 * `npx agent-share bench <ticket>` — bench consumer (transport from ticket).
 *
 * Folder receive writes real files rather than mounting: NFS is native-only.
 * Folder produce stays on the native binary (scan order is the READ index).
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
  console.error('usage:')
  console.error('  npx agent-share <ticket> [destination] [--password <pw>]')
  console.error('  npx agent-share bench --transport webrtc')
  console.error('  npx agent-share bench <ticket>')
  console.error()
  console.error('  Receive writes into `destination` (default: ./share).')
  console.error(
    '  --password (or AGENT_SHARE_PASSWORD) unlocks a password-protected share.',
  )
  console.error('  Produce a share with the native binary: agent-share serve <dir>')
  console.error('  Bench: producer sets --transport; consumer reads it from the ticket.')
}

/**
 * Install a WebRTC implementation onto `globalThis`, or explain why we cannot.
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

/**
 * Release `node-datachannel`'s native threads.
 *
 * Its addon holds a worker pool that keeps Node's event loop alive, so a
 * command that only closes the connection runs to completion and then never
 * exits. Best-effort: a missing addon means nothing was ever started.
 */
async function shutdownWebRtc() {
  try {
    const dc = /** @type {{ cleanup?: () => void }} */ (
      /** @type {unknown} */ (await import('node-datachannel'))
    )
    dc.cleanup?.()
  } catch {
    // Nothing to release.
  }
}

/**
 * Load the wasm client built by `cargo task web-wasm`.
 *
 * A package name rather than a path: it resolves through `node_modules` from a
 * checkout and from an installed tarball alike, so there is one spelling rather
 * than one per world. The catch turns a checkout that never built the wasm into
 * an instruction rather than a bare module-not-found.
 */
async function loadClient() {
  try {
    return await import('agent-share-wasm/node')
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
 * @param {string | undefined} password
 */
async function receive(ticket, destination, password) {
  await installWebRtc()
  const wasm = await loadClient()

  // Checked before the dial so a missing password reads as a usage error
  // rather than as a share that refuses to talk.
  if (wasm.ShareClient.password_required(ticket) && password === undefined) {
    throw new Error(
      'this share is password-protected — pass --password <pw> or set AGENT_SHARE_PASSWORD',
    )
  }

  process.stderr.write('connecting (webrtc)…\n')
  const client = await wasm.ShareClient.connect(
    ticket,
    'webrtc',
    undefined,
    undefined,
    password,
  )
  process.stderr.write(`connected over ${client.transport}\n`)
  const manifest = /** @type {Manifest} */ (await client.manifest())

  const root = resolve(destination)
  await mkdir(root, { recursive: true })

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

    let offset = 0
    const chunks = new Readable({
      async read() {
        if (offset >= file.size) {
          this.push(null)
          return
        }
        const want = Math.min(CHUNK, file.size - offset)
        const chunk = await client.read(index, BigInt(offset), want)
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

  // Node has no page to navigate away from: an open data channel keeps the
  // event loop alive, so a receive that does not close would never return.
  client.leave_mesh()
  client.close_connection()
  await shutdownWebRtc()
}

/**
 * @param {string} transport
 */
async function benchProduce(transport) {
  if (transport !== 'webrtc') {
    throw new Error('bench producer requires --transport webrtc')
  }
  await installWebRtc()
  const wasm = await loadClient()
  process.stderr.write(`starting bench producer (${transport})…\n`)
  const producer = await wasm.BenchProducer.start(transport)
  const ticket = producer.ticket
  console.log(`npx agent-share bench '${ticket}'`)
  process.stderr.write('bench producer running — Ctrl-C to stop\n')

  await new Promise((resolve) => {
    const stop = async () => {
      process.stderr.write('\nstopping…\n')
      try {
        await producer.stop()
      } catch {
        // best-effort
      }
      resolve(undefined)
    }
    process.once('SIGINT', () => {
      void stop()
    })
    process.once('SIGTERM', () => {
      void stop()
    })
  })
}

/**
 * @param {string[]} argv
 */
function parseBenchArgs(argv) {
  /** @type {string | undefined} */
  let ticket
  /** @type {string | undefined} */
  let transport
  /** @type {number | undefined} */
  let duration
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i]
    if (arg === '--transport') {
      transport = argv[++i]
      continue
    }
    if (arg.startsWith('--transport=')) {
      transport = arg.slice('--transport='.length)
      continue
    }
    if (arg === '--duration') {
      duration = Number(argv[++i])
      continue
    }
    if (arg.startsWith('--duration=')) {
      duration = Number(arg.slice('--duration='.length))
      continue
    }
    if (arg === '-h' || arg === '--help') {
      return { help: true }
    }
    if (!ticket) {
      ticket = arg
      continue
    }
    throw new Error(`unexpected argument: ${arg}`)
  }
  return { ticket, transport, duration, help: false }
}

/**
 * @param {string} ticket
 * @param {number | undefined} duration
 */
/**
 * @param {{ stage: string, transport?: string, connect_ms?: number, duration_s?: number, elapsed_s?: number }} status
 */
function onBenchStatus(status) {
  switch (status.stage) {
    case 'connecting':
      process.stderr.write(`connecting (${status.transport})…\n`)
      break
    case 'connected':
      process.stderr.write(
        `connected ${Number(status.connect_ms).toFixed(1)} ms (${status.transport})\n`,
      )
      break
    case 'benching':
      process.stderr.write(`benching ${status.duration_s}s…\n`)
      break
    case 'progress':
      process.stderr.write(`benching ${status.elapsed_s}s / ${status.duration_s}s\n`)
      break
    default:
      break
  }
}

/**
 * @param {string} ticket
 * @param {number} [duration] Seconds to bench for. Defaults inside the client.
 */
async function benchConsume(ticket, duration) {
  if (duration !== undefined && !(Number.isFinite(duration) && duration > 0)) {
    throw new Error('--duration must be a positive number of seconds')
  }
  await installWebRtc()
  const wasm = await loadClient()
  const report = await wasm.ShareClient.bench(
    ticket,
    duration === undefined ? undefined : BigInt(Math.floor(duration)),
    onBenchStatus,
  )
  console.log(JSON.stringify(report, null, 2))
  await shutdownWebRtc()
}

/**
 * Split the receive argv into positionals and the password.
 *
 * `--password <pw>` and `--password=<pw>` both work; `AGENT_SHARE_PASSWORD` is
 * the form for scripts, since piping into a bare `npx` invocation is awkward
 * and a literal on the command line lands in shell history.
 *
 * @param {string[]} argv
 * @returns {{ positional: string[], password: string | undefined }}
 */
function parseReceiveArgs(argv) {
  /** @type {string[]} */
  const positional = []
  let password = process.env.AGENT_SHARE_PASSWORD || undefined
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i]
    if (arg === '--password') {
      password = argv[++i]
      continue
    }
    if (arg.startsWith('--password=')) {
      password = arg.slice('--password='.length)
      continue
    }
    positional.push(arg)
  }
  return { positional, password }
}

async function main() {
  const argv = process.argv.slice(2)
  if (argv[0] === '-h' || argv[0] === '--help') {
    usage()
    process.exit(0)
  }

  if (argv[0] === 'bench') {
    const parsed = parseBenchArgs(argv.slice(1))
    if (parsed.help) {
      usage()
      process.exit(0)
    }
    if (!parsed.ticket) {
      if (!parsed.transport) {
        throw new Error('bench producer requires --transport webrtc')
      }
      await benchProduce(parsed.transport)
      return
    }
    if (parsed.transport) {
      throw new Error(
        'bench consumer has no --transport; the producer sets it in the ticket',
      )
    }
    await benchConsume(parsed.ticket, parsed.duration)
    return
  }

  const { positional, password } = parseReceiveArgs(argv)
  const [ticket, destination = './share'] = positional
  if (!ticket) {
    usage()
    process.exit(2)
  }
  await receive(ticket, destination, password)
}

main().catch((error) => {
  process.stderr.write(`\n${error.message ?? error}\n`)
  process.exit(1)
})
