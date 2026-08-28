/**
 * `bun run dev:docker`: build the container image and run it here, before it is
 * published.
 *
 * `bun run start` serves the checkout's `dist/`, which is not the same artefact:
 * the image rebuilds the wasm inside its build stage, carries a bundled
 * `serve.js` with no source tree beside it, and runs as `bun` on a read-only
 * filesystem. A `COPY` that missed a file, or a runtime that wants to write
 * somewhere, shows up here and nowhere else.
 *
 * The run flags below are `deploy/compose.yaml` transcribed. Relaxing any of
 * them would let a container pass locally and fail on the homelab, which is the
 * one thing this script exists to catch.
 *
 * `PORT` picks the host port, so this can run alongside `bun run dev` instead of
 * losing a coin flip for 3000. The container always listens on 3000.
 */

const IMAGE = 'agent-share-web:dev'
const CONTAINER = 'agent-share-web-dev'

const port = Number(process.env.PORT ?? 3000)

// The homelab and this Mac are both arm64, so the default emulates nothing.
const platform = process.env.DOCKER_PLATFORM ?? 'linux/arm64'

async function run(...argv: string[]) {
  const child = Bun.spawn(argv, { stdio: ['inherit', 'inherit', 'inherit'] })
  return await child.exited
}

// Docker Desktop stops between sessions, and the error it raises from inside a
// build reads as a network problem several steps deep.
if ((await Bun.spawn(['docker', 'info'], { stdout: 'ignore', stderr: 'ignore' }).exited) !== 0) {
  console.error('the Docker daemon is not reachable — start Docker and retry')
  process.exit(1)
}

// The build context is the repo root, not `scripts/`: the image builds the wasm
// from `crates/`, so `packages/` is only half of what it needs.
const root = new URL('..', import.meta.url).pathname
const built = await run('docker', 'build', '--platform', platform, '-t', IMAGE, root)
if (built !== 0) process.exit(built)

console.log(`dev:docker http://localhost:${port}`)

// A previous run that was killed rather than interrupted leaves the name taken,
// and `docker run --name` then refuses to start. The name is this script's own,
// so reclaiming it costs nothing.
await Bun.spawn(['docker', 'rm', '-f', CONTAINER], {
  stdout: 'ignore',
  stderr: 'ignore',
}).exited

const container = Bun.spawn(
  [
    'docker',
    'run',
    '--rm',
    ...(process.stdin.isTTY ? ['-it'] : []),
    '--name',
    CONTAINER,
    '-p',
    `${port}:3000`,
    '--read-only',
    '--tmpfs',
    '/tmp',
    '--cap-drop',
    'ALL',
    '--security-opt',
    'no-new-privileges:true',
    IMAGE,
  ],
  { stdio: ['inherit', 'inherit', 'inherit'] },
)

// `docker run` is a client, not the container: the process Ctrl-C reaches here
// is one end of a connection to the daemon, and how a signal travels the rest of
// the way depends on whether a TTY was allocated. Stopping it by name is the one
// path that does not, and waiting for the child afterwards keeps the shell
// prompt from returning over the top of the container's shutdown logs.
for (const signal of ['SIGINT', 'SIGTERM'] as const) {
  process.on(signal, () => {
    Bun.spawn(['docker', 'stop', '--time', '5', CONTAINER], {
      stdout: 'ignore',
      stderr: 'ignore',
    })
  })
}

process.exit(await container.exited)
