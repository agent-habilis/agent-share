# `agent-share` 🗄️

Share a folder with peers, or mount the folder of a peer on your machine, peer
to peer over [iroh](https://www.iroh.computer/). The mount is read-only and
lazy, with no daemon and no FUSE.

## Features

- **[Peer to peer](https://agent-share.dev/docs#no-server-no-daemon)** — the two peers connect directly. No server to host, no account to create.
- **[Encrypted](https://agent-share.dev/docs/networking#transports)** — bytes move over iroh QUIC, or WebRTC in a browser. Both are encrypted.
- **[Real folder](https://agent-share.dev/docs/concepts#the-nfs-bridge)** — the share mounts as a directory. grep it, open it in an editor, build against it.
- **[No FUSE](https://agent-share.dev/docs/concepts#the-nfs-bridge)** — the built-in NFS client of the OS does the mount. No kernel extension, no daemon.
- **[Lazy](https://agent-share.dev/docs/concepts#lazy-reads)** — a peer fetches only the byte ranges that it reads, when it reads them.
- **[Live](https://agent-share.dev/docs/concepts#live-changes)** — edits, new files and deletions reach connected peers without a remount.
- **[Read-only](https://agent-share.dev/docs/concepts#the-nfs-bridge)** — a consumer cannot change the files of the producer. Every write fails.
- **[Password-protected](https://agent-share.dev/docs/commands#passwords)** — with a password, the ticket finds the share but does not open it.
- **[Mirror](https://agent-share.dev/docs/concepts#mirror)** — download the full tree, verified against the origin, and serve it as a second source.
- **[Browser](https://agent-share.dev/docs/browser#the-webapp)** — open a share in the webapp, or create one from a folder. No install, no backend.
- **[npx](https://agent-share.dev/docs/browser#npx)** — `npx agent-share` writes the files into a local folder, with no native binary.
- **[WebMCP](https://agent-share.dev/docs/webmcp)** — the webapp publishes its actions as WebMCP tools, so an agent can drive it.
- **[Multi-machine](https://agent-share.dev/docs/networking#discovery)** — the local network over mDNS, the internet over the DHT and relays.
- **[macOS and Linux](https://agent-share.dev/docs/getting-started#platforms)** — mount works on macOS and Linux. Other platforms can serve.

## No server

The two peers connect directly. There is nothing to host and no account to
create. `serve` prints a ticket, and the ticket is the whole setup.

## Installation

```sh
# Homebrew for macOS and Linux
brew install agent-habilis/tap/agent-share

# From source everywhere else
cargo install --git https://github.com/agent-habilis/agent-share agent-share
```

For the supported platforms, see
[Getting started](https://agent-share.dev/docs/getting-started).

## Usage

```sh
agent-share serve <dir>          # producer: prints a ticket and the mount command
agent-share <ticket> <target>    # consumer: mounts the share under <target>
```

With no install, open the ticket in the
[webapp](https://agent-share.dev/app/), or run `npx agent-share <ticket>`.

## Links

- [Docs](https://agent-share.dev/docs)
- [Webapp](https://agent-share.dev/app/)
- [Contributing](CONTRIBUTING.md)
- [License](LICENSE)
- [agent-habilis](https://agent-habilis.com)
- [iroh](https://www.iroh.computer/)
