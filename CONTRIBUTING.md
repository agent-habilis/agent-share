# Contributing

Also see [docs/testing.md](docs/testing.md) for the test suites and
[docs/vendoring.md](docs/vendoring.md) for the vendored packages. The user
docs are in `packages/agent-share-site/content/docs/`.

## Development

```
cargo build
cargo task fmt
cargo clippy --all-targets
cargo test                          # includes a subprocess bridge test, no OS mount
cargo test --test mount -- --ignored  # the real OS-mount round trip, run by hand
```

Imports form one block at the top of the file, before any `mod`, in three
groups: std, then external crates (the workspace crates included), then
`self`/`super`/`crate`. Write a child-module import as `self::child::…` so that
it sorts into the last group. `cargo task fmt` runs nightly rustfmt (pinned in
`tasks/src/fmt.rs`) for the `group_imports` option in `rustfmt.toml`, and
`cargo task ci` fails on drift. rustfmt does not move a `use` that comes after
a `mod`.

The wire format is pinned by golden tests (`wire_constants_are_pinned`,
`type_bytes_are_pinned_wire_format`, `swarm_id_wire_format_is_pinned`) — if one
of those fails after a change, you broke compatibility with already-issued
tickets and with peers running an older build.

## Releasing

```
cargo task release minor              # dry run
cargo task release minor --execute    # bump, commit, annotated tag — no push
git push origin main --follow-tags
```

The tag push starts `.github/workflows/release.yml`. It builds the four
binaries, creates the GitHub release, and opens a PR that updates
`Formula/agent-share.rb`. When that PR merges into `main`,
`.github/workflows/tap.yml` copies the formula to `agent-habilis/homebrew-tap`.
That copy needs the `TAP_PUSH_TOKEN` Actions secret: a fine-grained PAT with contents
read/write on the tap repo.

## Building

```
bun install && bun run build
```

`packages/` is the JavaScript half — a Bun workspace beside the `crates/` cargo
one, with the same flat shape. `agent-share-web` is the whole browser half in one
package, over `agent-share-wasm`: the share logic under `src/lib/`, the UI kit
under `src/components/`, and the routes, service worker and lab that bundle them.
The logic is not a package of its own because almost none of it could be shared
anyway — it reaches for `showDirectoryPicker`, `navigator.serviceWorker` and the
OPFS. The exception is the stream protocol, which `scripts/serve.ts` imports by
path so the static host's 404 rule cannot drift from the service worker's URL
prefix. `agent-share-node` is the
`npx agent-share <ticket>` receiver; its directory name carries the
`agent-share-` prefix its npm name (`agent-share`) does not. The six `visage-*`
and `moonspace-*` members are vendored — see `docs/vendoring.md`.

Browser and receiver consume the same `.wasm`; only the wasm-bindgen glue
differs, and `scripts/build-wasm.ts` writes both layers into
`agent-share-wasm` — the browser's ES-module glue into `src/glue/`, the
receiver's CommonJS glue into `node/` beside the binary it reads at import time.
So `agent-share-node` imports `agent-share-wasm/node` by name rather than
reaching into a build directory, and those two are the members that get
published: the receiver, and the wasm package it depends on.

`bun run build` builds that wasm too, through `scripts/build-wasm.ts` — so
it needs the `wasm32-unknown-unknown` target, the `wasm-bindgen` CLI, and a
clang that can emit wasm32 (`brew install llvm` on macOS; Apple's has no wasm
backend). It says which one is missing. `cargo task web-wasm` runs the same
script when only the binary is wanted.

`bun run dev` serves at `https://agent-share.localhost` — a name instead of a
contended port, via [portless](https://github.com/vercel-labs/portless) — and
expects the wasm to exist already. It builds the landing page and docs
(`packages/agent-share-site`, Next + Nextra) once, then serves them around the
app at `/app`, which hot-reloads. For live docs editing, run `bun run dev` in
`packages/agent-share-site`. `bun run build && bun run start` is the
production pair: `start` serves the built `dist/` on `PORT`, with the same
routing the deployed image uses (`scripts/serve.ts`).

## Deploying

```
bun run dev:docker           # build the image and run it here, on :3000
cargo task web-image         # build linux/arm64, push to the Gitea registry
```

The image is Bun serving `dist/`, and it rebuilds the wasm from source rather
than copying a local `dist/`, so it cannot ship a stale binary. `deploy/compose.yaml`
is what runs on the host — read its header first: the app needs a secure context
(HTTPS or localhost) or media previews silently lose the ability to seek.

The static host must fall back to `app/index.html` for deep links under
`/app`, because the ticket is in the path.
