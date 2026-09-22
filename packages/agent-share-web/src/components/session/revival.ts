/**
 * What the reconnect loop is up to, for the parts of the page that show it.
 *
 * Non-null for exactly as long as `ensureLive` runs. The loop retries forever,
 * so without this a dead origin looked the same as a healthy share: every
 * peer action disabled, the header still drawing the old client's counts, and
 * no word about why. The attempt count and the last error are what a person
 * needs to tell "one more second" from "the producer is gone".
 */
export interface Revival {
  /** Attempts that have failed so far; 0 while the first one runs. */
  readonly attempts: number
  /** Why the last attempt failed, or null before any has. */
  readonly lastError: string | null
}

/** Tooltip text for a control held back by the redial. */
export function describeRevival(revival: Revival): string {
  const head =
    revival.attempts === 0
      ? 'Reconnecting — the connection dropped while the tab was away.'
      : `Reconnecting — ${revival.attempts} ${revival.attempts === 1 ? 'attempt' : 'attempts'} failed so far.`
  return revival.lastError ? `${head}\nLast error: ${revival.lastError}` : head
}
