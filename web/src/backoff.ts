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
