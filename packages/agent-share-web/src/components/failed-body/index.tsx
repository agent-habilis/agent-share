import { Box, Stack, Text } from 'moonspace-dom'

import { Centered } from '../centered/index.tsx'

export function FailedBody({ reason }: { reason: string }) {
  const iceHint = /ice_connection_state|ondatachannel|ICE failed|no ICE candidates/i.test(reason)
  return (
    <Centered>
      <div style={{ padding: '0 2ch', maxWidth: '60ch' }}>
        <Box border="line" padX={2} padY={1}>
          <Stack direction="column" gap={1}>
            <Text weight="bold" color="danger">
              Could not connect
            </Text>
            <Text color="fgMuted">
              {iceHint
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
