import { Box, Stack, Text } from 'moonspace-dom'

import { Centered } from '../Centered/index.tsx'

/**
 * `unsupported` is a missing browser capability, not a connection problem —
 * retrying or opening a firewall will never help, so it gets its own copy.
 */
export type FailureKind = 'unsupported'

export function FailedBody({ reason, kind }: { reason: string; kind?: FailureKind }) {
  const unsupported = kind === 'unsupported'
  const iceHint =
    !unsupported &&
    /ice_connection_state|ondatachannel|ICE failed|no ICE candidates/i.test(reason)
  return (
    <Centered>
      <div style={{ padding: '0 2ch', maxWidth: '60ch' }}>
        <Box border="line" padX={2} padY={1}>
          <Stack direction="column" gap={1}>
            <Text weight="bold" color="danger">
              {unsupported ? 'Not supported in this browser' : 'Could not connect'}
            </Text>
            <Text color="fgMuted">
              {unsupported
                ? 'Sharing a folder needs the File System Access API, which Safari and Firefox do not implement. Receiving a share works here; to send one, use Chrome, Edge, or another Chromium browser.'
                : iceHint
                  ? 'WebRTC could not open a path between the two browsers (LAN/mDNS and TURN both failed). On macOS, allow Local Network for this browser under System Settings → Privacy & Security → Local Network, hard-refresh both tabs, and retry.'
                  : 'A direct connection to this peer could not be established. Both ends may be behind restrictive NATs.'}
            </Text>
            {/* The raw error, which is exactly what gets pasted into a report. */}
            <Text color="fgSubtle" class="selectable">
              {reason}
            </Text>
          </Stack>
        </Box>
      </div>
    </Centered>
  )
}
