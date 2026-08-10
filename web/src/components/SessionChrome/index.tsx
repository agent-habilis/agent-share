import { Text } from 'moonspace-dom'
import type { Child } from 'visage-dom'

import { Chrome } from '../Chrome/index.tsx'
import { TransferStatus } from '../TransferStatus/index.tsx'
import type { SessionApi } from '../Session/session.ts'

/**
 * The app chrome as a share page wears it.
 *
 * The breadcrumb and the trailing actions name where you are, so each page
 * passes its own. The two slots between them are the same on every share page
 * and are filled from the session here, rather than three times over.
 */
export function SessionChrome({
  session,
  crumb,
  trailing,
  children,
}: {
  session: SessionApi
  crumb: string
  trailing?: Child
  children: Child
}) {
  const active = session.transfer.value
  const mountErr = session.mountError.value
  const downloadErr = session.downloadError.value
  const seedErr = session.seedError.value
  const skipped = session.ready.value?.skipped ?? 0

  const belowBar =
    mountErr || downloadErr || seedErr || skipped > 0 ? (
      <>
        {/* All four are diagnostics, so all four stay copyable — see `app.css`. */}
        {mountErr ? (
          <Text color="danger" class="selectable">
            {mountErr}
          </Text>
        ) : null}
        {downloadErr ? (
          <Text color="danger" class="selectable">
            {downloadErr}
          </Text>
        ) : null}
        {seedErr ? (
          <Text color="danger" class="selectable">
            {seedErr}
          </Text>
        ) : null}
        {skipped > 0 ? (
          <Text color="warning" class="selectable">
            {skipped} entries hidden — unsafe paths in the peer&apos;s manifest
          </Text>
        ) : null}
      </>
    ) : null

  return (
    <Chrome
      crumb={crumb}
      center={
        /*
          Dropped while a transfer runs: that branch already gives the whole
          row to a `ProgressBar`, which answers "what is moving" better than a
          rate does, and two answers competing for one row is how the row stops
          being one row.

          Always up otherwise, including while re-dialling: the numbers are
          wire truth, and 000 KB/s against a dead peer is exactly what is
          happening. The old blank-while-redialling treatment made the whole
          bar vanish, which read as a broken page — worse than an honest zero.
        */
        active ? null : <TransferStatus sample={session.sample} />
      }
      trailing={trailing}
      belowBar={belowBar}
    >
      {children}
    </Chrome>
  )
}
