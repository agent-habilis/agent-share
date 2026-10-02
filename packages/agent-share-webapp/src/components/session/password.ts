/**
 * Where a protected share's password lives for the life of a tab.
 *
 * `sessionStorage`, keyed by a digest of the ticket. Three properties, each
 * chosen against a specific alternative:
 *
 * - **Not the URL.** The whole point of a password-protected share is that the
 *   link is safe to post where the password is not. A `#pw=` fragment would put
 *   both in the same string and in browser history.
 * - **Not `localStorage`.** A password that survives closing the tab outlives
 *   the reason it was typed, on a machine that may not be the typist's.
 * - **Not in-memory only.** A refresh, or switching `/files` ↔ `/info`, would
 *   re-prompt *and* re-pay the ~100 ms Argon2id — for no gain, since a tab that
 *   already holds a live connection to the share has the credential anyway.
 *
 * The value beside the key is the password in the clear. That is not an
 * oversight: `sessionStorage` has no encrypted tier, and anything with script
 * access to this origin can reach the live connection anyway. The key is
 * digested only so a glance at devtools storage does not read back *which*
 * shares this tab has open — which is why a non-cryptographic hash is enough,
 * and why it must stay **synchronous**: the session dials at construction, and
 * awaiting WebCrypto here would push the dial behind a microtask and break the
 * promise-identity check the client cache uses to release the right session.
 */

/** Namespace for every key this module writes. */
const PREFIX = 'as:pw:'

/**
 * FNV-1a over `ticket`, hex. A storage-key digest, not a commitment: collisions
 * across the handful of shares one tab holds are not a thing that happens, and
 * nothing downstream trusts this value.
 */
function keyFor(ticket: string): string {
  let hash = 0x811c9dc5
  for (let index = 0; index < ticket.length; index += 1) {
    hash ^= ticket.charCodeAt(index)
    // The FNV prime, as the shift-add form that stays inside 32 bits in JS.
    hash = (hash + ((hash << 1) + (hash << 4) + (hash << 7) + (hash << 8) + (hash << 24))) >>> 0
  }
  return PREFIX + hash.toString(16).padStart(8, '0')
}

/**
 * Storage, or `null` where it is unavailable.
 *
 * Safari in private mode and some embedded webviews throw on access rather than
 * returning null. A tab that cannot remember a password still works — it just
 * asks again — so this must never be the thing that breaks the page.
 */
function store(): Storage | null {
  try {
    return window.sessionStorage
  } catch {
    return null
  }
}

/** The password remembered for `ticket` in this tab, if any. */
export function rememberedPassword(ticket: string): string | undefined {
  try {
    return store()?.getItem(keyFor(ticket)) ?? undefined
  } catch {
    return undefined
  }
}

/** Remember `password` for `ticket` for the life of this tab. */
export function rememberPassword(ticket: string, password: string): void {
  try {
    store()?.setItem(keyFor(ticket), password)
  } catch {
    // A full or disabled store costs a re-prompt, nothing more.
  }
}

/**
 * Forget `ticket`'s password.
 *
 * Called when the producer refuses it: a remembered wrong password would
 * otherwise be retried silently on every reload, and the gate would never
 * reappear to let anyone fix it.
 */
export function forgetPassword(ticket: string): void {
  try {
    store()?.removeItem(keyFor(ticket))
  } catch {
    // Nothing to do — the next connect simply tries the stale value again.
  }
}
