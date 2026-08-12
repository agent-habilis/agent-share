/**
 * Hand bytes that just arrived back to the client, so this tab can seed them.
 *
 * Every path that pulls a file — a download, a folder-as-ZIP, a mirror to a
 * local folder, a preview buffered into a Blob, a preview streamed through the
 * service worker — pulls the same bytes over the same connection. Without this
 * they were read once and thrown away, and pressing Seed afterwards pulled the
 * whole file a second time.
 *
 * Kept in one module rather than copied per caller because the contract is the
 * interesting part, and it is easy to get subtly wrong: a fourth copy that
 * forgot the synchronous guard below would turn a full disk into a failed
 * download.
 */

/** The half of a reader this module needs. Optional, and never load-bearing. */
export interface Keeper {
  keep?(index: number, offset: bigint, bytes: Uint8Array): Promise<void>
}

/**
 * Keeps still in flight, so a publish can wait for them.
 *
 * Held as a set rather than a count because entries remove themselves on
 * settle, and a count would have to be decremented from two places.
 */
const outstanding = new Set<Promise<void>>()

/**
 * Feed fetched bytes to the client's chunk store, and never let it matter.
 *
 * Fire-and-forget on purpose, in both directions:
 *
 * - **Not awaited**, so storing never sits between two reads and slows a
 *   transfer down. The bytes are already in hand; keeping them is bookkeeping.
 * - **Never rethrown**, so a full quota or a private-mode refusal costs seeding
 *   and not the transfer. A user who asked for a file gets the file.
 *
 * The `try` around the call itself is not belt-and-braces. `keep` crosses into
 * wasm, which allocates before it copies, and an allocation failure throws
 * *synchronously* — before there is a promise for `.catch` to attach to. Left
 * uncaught it escapes into the caller's `pull`, which errors the very stream
 * this is promising never to disturb.
 *
 * The client keeps only chunks lying wholly inside what it is given, so callers
 * are expected to read on chunk boundaries — see `step` in `stream/range.ts`
 * for the one path where that took arranging.
 */
export function keepChunks(
  keeper: Keeper,
  index: number,
  offset: number,
  bytes: Uint8Array,
): void {
  if (!keeper.keep) return
  try {
    const pending = keeper.keep(index, BigInt(offset), bytes).catch((error: unknown) => {
      console.debug('[share] keeping a chunk failed; not seeding these bytes', error)
    })
    outstanding.add(pending)
    void pending.finally(() => outstanding.delete(pending))
  } catch (error) {
    console.debug('[share] keeping a chunk failed; not seeding these bytes', error)
  }
}

/**
 * Resolve once every keep issued so far has landed.
 *
 * Publishing what this tab holds reads the chunk store, so publishing while
 * keeps are still in flight advertises a file as less complete than it is —
 * and the correction only arrives with the next publish, which may be never.
 *
 * Deliberately not a barrier for anything else: keeps issued *after* this is
 * called are not waited for, because the caller is on its way out and the
 * alternative is a promise that never settles under a live transfer.
 */
export async function keepsSettled(): Promise<void> {
  await Promise.all(Array.from(outstanding))
}
