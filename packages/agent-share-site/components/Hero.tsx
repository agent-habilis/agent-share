import { readFile } from 'node:fs/promises'
import { join } from 'node:path'

const GITHUB = 'https://github.com/agent-habilis/agent-share'

// Read from the crate rather than typed here, so the chip cannot drift from
// the binary's version. A cwd path rather than import.meta.url: Turbopack turns `new URL(…, import.meta.url)`
// into an asset import and refuses a file outside the package.
async function crateVersion(): Promise<string> {
  const manifest = await readFile(join(process.cwd(), '../../crates/agent-share/Cargo.toml'), 'utf8')
  return /^version\s*=\s*"([^"]+)"/m.exec(manifest)?.[1] ?? '0.0.0'
}

export async function Hero() {
  const version = await crateVersion()
  return (
    <section className="hero">
      <div className="hero-text">
        <a className="chip" href={`${GITHUB}/releases`} target="_blank" rel="noopener">
          v{version}
        </a>
        <h1 className="hero-title">
          <span className="hero-accent">Share a folder.</span>
          <br />
          Mount it anywhere.
        </h1>
        <p className="hero-sub">Peer to peer and read-only. Bytes move only when someone reads them. No daemon, no FUSE, no account.</p>
        <p className="hero-actions">
          <a className="btn btn-primary" href="/docs/getting-started">
            Get started →
          </a>
          <a className="hero-link" href={GITHUB} target="_blank" rel="noopener">
            GitHub ↗
          </a>
        </p>
      </div>
      {/* The brand mark is the emoji itself — the favicon is the same glyph —
          so the hero shows it as the image rather than a logo
          that does not exist. */}
      <span className="hero-mark" aria-hidden="true">
        🗄️
      </span>
    </section>
  )
}
