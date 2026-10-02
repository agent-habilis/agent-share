/**
 * De-phase a retry delay: every tab of a share notices the producer die
 * within the same 1 s liveness tick, and a deterministic ladder then retries
 * in lockstep volleys — synchronized JSEP rounds and mesh dials from the
 * whole swarm at 1, 2, 4 … 30 s, forever, each volley minting the churned
 * relay identities the roster already suffers from. One multiplier in
 * [0.5, 1.5) spreads a volley across a window as wide as the delay itself
 * without changing the ladder's average pace, so the backoff-ceiling math
 * (~120 polite attempts per hour of dead origin) still holds.
 */
export function jittered(ms: number): number {
  return ms * (0.5 + Math.random())
}

/**
 * The tight origin cap a revival attempt starts with: the connection just
 * died, so the producer is almost certainly gone and the seeder lane should
 * get the attempt rather than a doomed dial.
 */
export const REVIVAL_ORIGIN_CAP_MS = 8_000

/**
 * How many attempts the tight cap is worth betting before the origin gets
 * its full dial back. Three covers the window where "it just died" is still
 * a fair guess, and the ladder reaches it in a few seconds.
 */
const TIGHT_REVIVAL_ATTEMPTS = 3

/**
 * The origin-dial cap for revival attempt `attempt` (0-based), or
 * `undefined` to let the wasm side use its patient default.
 *
 * The tight cap is a bet that the origin is dead, and it has to be a bet
 * rather than a standing rule: a producer that comes back on a slow link
 * needs the measured 6-20 s to answer, which 8 s cuts off every time. A loop
 * that passed the same 8 s to every attempt could never reach that producer
 * again, so the tab stayed on frozen seeder snapshots for as long as it was
 * open. Once the bet has lost `TIGHT_REVIVAL_ATTEMPTS` times, stop making
 * it.
 */
export function revivalOriginCapMs(attempt: number): number | undefined {
  return attempt < TIGHT_REVIVAL_ATTEMPTS ? REVIVAL_ORIGIN_CAP_MS : undefined
}
