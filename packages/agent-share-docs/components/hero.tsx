const GITHUB = 'https://github.com/agent-habilis/agent-share'

export async function Hero() {
  return (
    <section className="hero">
      <div className="hero-text">
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
