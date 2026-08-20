import { Spinner, Stack, Text } from 'moonspace-dom'

import { Centered } from '../Centered/index.tsx'

export function LoadingBody({ label }: { label: string }) {
  return (
    <Centered>
      <Stack direction="row" gap={1}>
        <Spinner />
        <Text>{label}</Text>
      </Stack>
    </Centered>
  )
}
