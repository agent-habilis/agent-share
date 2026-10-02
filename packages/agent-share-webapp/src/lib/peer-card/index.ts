/**
 * Peer identity the web consumer publishes onto the share mesh meta card.
 *
 * Wasm only relays what we pass — runtime detection lives here so Safari and
 * Chrome each advertise correctly from their own tab.
 */

/** Keep in lockstep with `crates/agent-share-wasm-client` / proto package version. */
export const SHARE_VERSION = '0.1.0'

export type PeerRole = 'consumer' | 'producer'

export interface PeerCardInput {
  version: string
  runtime: string
  transport?: string
  role?: PeerRole
}

/** Best-effort browser family from `navigator.userAgent`. */
export function detectRuntime(userAgent = navigator.userAgent): string {
  const ua = userAgent.toLowerCase()
  if (ua.includes('edg/') || ua.includes('edgios/')) return 'edge'
  if (ua.includes('chrome/') || ua.includes('crios/')) return 'chrome'
  if (ua.includes('firefox/') || ua.includes('fxios/')) return 'firefox'
  if (ua.includes('safari/')) return 'safari'
  return 'browser'
}

export function buildPeerCard(opts: {
  role: PeerRole
  /** Omit to let wasm fill from the mount data path / producer default. */
  transport?: string
  runtime?: string
  version?: string
}): PeerCardInput {
  const card: PeerCardInput = {
    version: opts.version ?? SHARE_VERSION,
    runtime: opts.runtime ?? detectRuntime(),
    role: opts.role,
  }
  if (opts.transport) card.transport = opts.transport
  return card
}
