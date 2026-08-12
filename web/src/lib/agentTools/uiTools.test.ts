import { afterEach, describe, expect, test } from 'bun:test'

import { UI_TOOLS } from './uiTools.ts'
import { publishAgentSession, type AgentSession, type ShareViewName } from './uiBridge.ts'

const tool = (name: string) => {
  const found = UI_TOOLS.find((entry) => entry.name === name)
  if (!found) throw new Error(`no ${name} tool`)
  return found
}

interface Fake extends AgentSession {
  calls: string[]
}

function fakeSession(overrides: Partial<AgentSession> = {}): Fake {
  const calls: string[] = []
  let selection: string[] = []
  let view: ShareViewName = 'files'
  let errors = { download: null as string | null, mount: null as string | null, seed: null as string | null }

  return {
    calls,
    ticket: 'TICKET',
    selection: () => selection,
    select: (next) => {
      calls.push(`select:${next.join('/')}`)
      selection = next
    },
    view: () => view,
    openView: (next, file) => {
      calls.push(`openView:${next}:${(file ?? []).join('/')}`)
      view = next
    },
    status: () => 'ready',
    mounted: () => false,
    transfer: () => null,
    errors: () => errors,
    download: () => calls.push('download'),
    mount: () => calls.push('mount'),
    seed: () => calls.push('seed'),
    ...overrides,
    /** Test-only hook so a case can make an action fail after it is triggered. */
    ...({ __setErrors: (next: typeof errors) => (errors = next) } as object),
  } as Fake
}

let release: (() => void) | undefined
afterEach(() => {
  release?.()
  release = undefined
})

function mount(session: AgentSession): void {
  release = publishAgentSession(session)
}

describe('with no share page open', () => {
  // The whole point of the bridge being withdrawn on unmount: on `/` there is
  // no session, and inventing one would move a page nobody is looking at.
  test.each(UI_TOOLS.map((entry) => [entry.name] as const))(
    '%s says so rather than guessing',
    async (name) => {
      const result = (await tool(name).execute({})) as { ok: boolean; code?: string }

      expect(result.ok).toBe(false)
      expect(result.code).toBe('no_session')
    },
  )
})

describe('shareNavigate', () => {
  test('moves the browser and reports where it landed', async () => {
    const session = fakeSession()
    mount(session)

    const result = (await tool('shareNavigate').execute({ path: 'src/deep' })) as {
      ok: boolean
      selection: string[]
    }

    expect(result.ok).toBe(true)
    expect(session.calls).toContain('select:src/deep')
    expect(result.selection).toEqual(['src', 'deep'])
  })

  test('an empty path goes back to the share root', async () => {
    const session = fakeSession()
    mount(session)
    await tool('shareNavigate').execute({ path: '' })

    expect(session.calls).toContain('select:')
  })

  test('a path escaping the share never reaches the UI', async () => {
    const session = fakeSession()
    mount(session)

    const result = (await tool('shareNavigate').execute({ path: '../secret' })) as {
      ok: boolean
      code?: string
    }

    expect(result).toMatchObject({ ok: false, code: 'bad_argument' })
    expect(session.calls).toEqual([])
  })
})

describe('shareOpenView', () => {
  test('switches view', async () => {
    const session = fakeSession()
    mount(session)
    const result = (await tool('shareOpenView').execute({ view: 'info' })) as { ok: boolean }

    expect(result.ok).toBe(true)
    expect(session.calls.some((call) => call.startsWith('openView:info'))).toBe(true)
  })

  test('refuses a view it does not have', async () => {
    mount(fakeSession())
    const result = (await tool('shareOpenView').execute({ view: 'settings' })) as {
      ok: boolean
      code?: string
    }

    expect(result).toMatchObject({ ok: false, code: 'bad_argument' })
  })

  // Preview renders one file, so with nothing selected there is nothing to show.
  test('refuses to preview with nothing selected', async () => {
    mount(fakeSession())
    const result = (await tool('shareOpenView').execute({ view: 'preview' })) as {
      ok: boolean
      error?: string
    }

    expect(result.ok).toBe(false)
    expect(result.error).toMatch(/needs a file/)
  })

  test('previews the current selection when given no path', async () => {
    const session = fakeSession()
    mount(session)
    await tool('shareNavigate').execute({ path: 'src/lib.rs' })
    await tool('shareOpenView').execute({ view: 'preview' })

    expect(session.calls).toContain('openView:preview:src/lib.rs')
  })
})

describe('actions that open a file dialog', () => {
  /**
   * The honest failure. `showSaveFilePicker` and `showDirectoryPicker` need a
   * real click, and no directory handle is persisted, so there is nothing to
   * reuse — an agent driving this unattended has to be told to hand back to a
   * person rather than left to retry.
   */
  test('a refused picker is reported as needing a person, not as a generic failure', async () => {
    const session = fakeSession()
    const withError = {
      ...session,
      errors: () => ({
        download: 'NotAllowedError: user activation is required',
        mount: null,
        seed: null,
      }),
    }
    // The error is present from the start here, so it reads as pre-existing.
    // Publish a session whose error appears only after the action runs.
    let triggered = false
    mount({
      ...withError,
      errors: () =>
        triggered
          ? { download: 'NotAllowedError: user activation is required', mount: null, seed: null }
          : { download: null, mount: null, seed: null },
      download: () => {
        triggered = true
      },
    })

    const result = (await tool('shareDownload').execute({})) as { ok: boolean; code?: string; error?: string }

    expect(result).toMatchObject({ ok: false, code: 'needs_user_gesture' })
    expect(result.error).toMatch(/real click|person/)
  }, 10_000)

  test('an action that raises nothing reports as started', async () => {
    const session = fakeSession()
    mount(session)

    const result = (await tool('shareSeedSelection').execute({})) as { ok: boolean; started?: boolean }

    expect(result).toMatchObject({ ok: true, started: true })
    expect(session.calls).toContain('seed')
  }, 10_000)

  // An error already on screen when the tool ran was not caused by it.
  test('a pre-existing error is not blamed on this call', async () => {
    const stale = { download: 'an older failure', mount: null, seed: null }
    const session = fakeSession()
    mount({ ...session, errors: () => stale })

    const result = (await tool('shareDownload').execute({})) as { ok: boolean }

    expect(result.ok).toBe(true)
  }, 10_000)
})

describe('shareUiState', () => {
  test('reports what is on screen', async () => {
    mount(fakeSession())
    await tool('shareNavigate').execute({ path: 'src' })

    const result = (await tool('shareUiState').execute({})) as Record<string, unknown>

    expect(result).toMatchObject({
      ok: true,
      ticket: 'TICKET',
      view: 'files',
      selection: ['src'],
      status: 'ready',
      mounted: false,
    })
  })
})
