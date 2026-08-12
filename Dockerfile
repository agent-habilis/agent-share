# syntax=docker/dockerfile:1.7
#
# The web app as a container image: Bun handing out a static `dist/`.
#
# Hermetic by construction. The wasm the app loads is rebuilt from
# `crates/agent-share-wasm-client/` here rather than copied off a developer's
# machine, so the image cannot ship whatever `dist/` happened to be lying around
# in a checkout — the staleness class `web/scripts/wasm-asset.ts` documents at
# length. That is also why the build context is the repo root and not `web/`.
#
# Nothing below restates how the wasm is built: `bun run build` does that
# itself, through `web/scripts/build-wasm.ts`. This file only has to supply the
# toolchain that script expects to find.
#
# Driven by `cargo task web-image` (tasks/src/web_image.rs).

# --------------------------------------------------------------------------
# Stage 1 — the whole build: wasm binary, glue, bundle, and the server.
# --------------------------------------------------------------------------
FROM rust:1.95-bookworm AS build

# Pinned, and lifted from the official image rather than piped from an install
# script. `bun.lock` is lockfileVersion 1; a Bun that wants to migrate it would
# fail `--frozen-lockfile`, so this tracks the version developers run.
COPY --from=oven/bun:1.3.14 /usr/local/bin/bun /usr/local/bin/bun

# `ring`'s C core is compiled for wasm32 and gcc cannot emit it; Debian's clang
# can, which is what `build-wasm.ts` looks for on PATH. `llvm` supplies
# `llvm-ar`, since GNU `ar` does not understand wasm objects.
RUN apt-get update \
 && apt-get install -y --no-install-recommends clang llvm \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Ahead of any source: rustup reads this file and materialises the pinned
# toolchain on the first cargo call, and that download should not be repeated
# for a source edit.
COPY rust-toolchain.toml ./
RUN rustup show && rustup target add wasm32-unknown-unknown

# The prebuilt CLI, because `cargo install wasm-bindgen-cli` is minutes of
# compiling on every cache miss. Keep this in step with the `wasm-bindgen`
# version in `crates/agent-share-wasm-client/Cargo.lock` — wasm-bindgen checks
# its own schema against the binary's and fails loudly when they diverge, and
# `build-wasm.ts` prints the install line to fix it.
ARG WASM_BINDGEN_VERSION=0.2.126
RUN set -eux; \
    triple="$(uname -m)-unknown-linux-gnu"; \
    curl -fsSL "https://github.com/wasm-bindgen/wasm-bindgen/releases/download/${WASM_BINDGEN_VERSION}/wasm-bindgen-${WASM_BINDGEN_VERSION}-${triple}.tar.gz" \
      | tar -xz -C /usr/local/bin --strip-components=1 --wildcards '*/wasm-bindgen'; \
    wasm-bindgen --version

# Load-bearing, not incidental: this file carries
# `--cfg getrandom_backend="wasm_js"` for wasm32, without which getrandom
# refuses to build at all. Cargo finds it by walking up from the working
# directory — a plain directory walk that a nested `[workspace]` does not stop —
# so `/app/crates/agent-share-wasm-client` reaches `/app/.cargo/config.toml`.
COPY .cargo/config.toml .cargo/config.toml

# The wasm client is its own workspace and is never built from the root, but its
# path deps take `edition.workspace = true` from the ROOT manifest, so the root
# workspace has to be well formed. `members` names `tasks` as a literal path, so
# it has to exist even though nothing here compiles it.
COPY Cargo.toml Cargo.lock ./
COPY tasks/ tasks/
COPY crates/ crates/

WORKDIR /app/web

# Manifests first, so an edit under `src/` does not redo the install. `vendor/`
# comes along whole: its six packages are workspace members named with
# `workspace:*`, and `--frozen-lockfile` fails if their manifests are absent.
COPY web/package.json web/bun.lock ./
COPY web/vendor/ vendor/
RUN bun install --frozen-lockfile

COPY web/ ./

# One step, because `bun run build` is self-contained: it builds the wasm, the
# glue, the bundle, `sw.js`, and the content-addressed binary with its
# precompressed siblings. A `web/src/**` edit invalidates this layer, but the
# cargo half is then a no-op off the cache mount rather than a rebuild.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/crates/agent-share-wasm-client/target \
    bun run build

# Bundled rather than copied with its imports: `STREAM_PREFIX` gets inlined from
# the very file the service worker compiles against, so the 404 rule cannot
# drift from the protocol, and the runtime image carries no source tree.
RUN bun build ./scripts/serve.ts --target=bun --outfile=/app/out/scripts/serve.js

# --------------------------------------------------------------------------
# Stage 2 — runtime.
# --------------------------------------------------------------------------
FROM oven/bun:1.3.14-slim AS runtime
WORKDIR /app

ENV PORT=3000

# The checkout's layout, reproduced: `serve.js` resolves `../dist/` against its
# own module URL, so `scripts/` beside `dist/` is what makes it need no
# configuration.
COPY --from=build --chown=bun:bun /app/web/dist/ ./dist/
COPY --from=build --chown=bun:bun /app/out/scripts/serve.js ./scripts/serve.js

USER bun
EXPOSE 3000

# `/` is the SPA shell, read from `dist/` per request — so this fails when
# `dist/` is missing or unreadable, not merely when the process has died.
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
  CMD ["bun", "-e", "process.exit((await fetch(`http://127.0.0.1:${process.env.PORT ?? 3000}/`)).ok ? 0 : 1)"]

CMD ["bun", "./scripts/serve.js"]
