import type { Child } from 'visage-dom'

import { Chrome } from '../Chrome/index.tsx'
import { Toast } from '../Toast/index.tsx'
import { TransferStatus } from '../TransferStatus/index.tsx'
import type { SessionApi } from '../Session/session.ts'

/**
 * The app chrome as a share page wears it.
 *
 * The breadcrumb and the trailing actions name where you are, so each page
 * passes its own. What the session has to say — the live transfer rate, and
 * whatever last went wrong — is the same on every share page, and is filled in
 * here rather than three times over.
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
  const message = session.toast.value

  return (
    <Chrome
      crumb={crumb}
      toast={message ? <Toast {...message} onClose={session.dismissToast} /> : null}
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
    >
      {children}
    </Chrome>
  )
}
