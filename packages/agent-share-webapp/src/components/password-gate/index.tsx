import { Box, Button, Input, Stack, Text } from 'moonspace-dom'

import { Centered } from '../centered/index.tsx'

/**
 * The gate in front of a password-protected share.
 *
 * Rendered instead of the file browser, not over it: there is nothing behind it
 * to see yet. Until a password the producer accepts arrives, this tab has not
 * connected, is not on the share's mesh, and holds no listing — the ticket in
 * the URL is an address, not a key.
 */
export function PasswordGate({
  error,
  onSubmit,
}: {
  error?: string
  onSubmit: (password: string) => void
}) {
  let typed = ''
  const submit = () => {
    // An empty password is not a password — sending it would spend ~100 ms of
    // Argon2id and a round trip to be told what we already know.
    if (typed.length > 0) onSubmit(typed)
  }
  // A real `<form>` rather than a keydown handler on the input: Enter-to-submit
  // then comes from the platform, and the browser's password managers recognise
  // the shape and offer to fill it.
  return (
    <Centered>
      <div style={{ padding: '0 2ch', maxWidth: '60ch' }}>
        <Box border="line" padX={2} padY={1}>
          <form
            onsubmit={(event: Event) => {
              event.preventDefault()
              submit()
            }}
          >
            <Stack direction="column" gap={1}>
              <Text weight="bold">Password required</Text>
              <Text color="fgMuted">
                This share is password-protected. The link alone will not open it — ask
                whoever sent it for the password.
              </Text>
              <Input
                type="password"
                name="password"
                placeholder="Password"
                aria-label="Share password"
                invalid={error !== undefined}
                oninput={(event: Event) => {
                  typed = (event.target as HTMLInputElement).value
                }}
              />
              {error !== undefined ? (
                <Text color="danger" class="selectable">
                  {error}
                </Text>
              ) : null}
              <Button onclick={submit}>Open share</Button>
            </Stack>
          </form>
        </Box>
      </div>
    </Centered>
  )
}
