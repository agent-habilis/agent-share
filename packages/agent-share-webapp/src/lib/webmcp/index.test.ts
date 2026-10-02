import { describe, expect, test } from 'bun:test'

import { registerAgentTools, TOOLS } from './index.ts'

/**
 * The tool definitions are the whole contract with an agent, and nothing
 * downstream checks them: the browser accepts any `inputSchema` without reading
 * it, and it does not validate a call against one. A schema that is wrong is
 * therefore wrong silently, in the one place a model is relying on it.
 */

interface Schema {
  type: string
  properties: Record<string, { type?: string; enum?: string[]; description?: string }>
  required?: string[]
  additionalProperties?: boolean
}

const schemaOf = (tool: ModelContextTool) => tool.inputSchema as unknown as Schema

describe('tool names', () => {
  // 1–128 characters of ASCII alphanumeric, `_`, `-` or `.`, per the WebMCP IDL.
  test.each(TOOLS.map((tool) => [tool.name] as const))('%s is a legal WebMCP name', (name) => {
    expect(name).toMatch(/^[A-Za-z0-9_.-]{1,128}$/)
  })

  test('names are unique — a duplicate is rejected with InvalidStateError', () => {
    expect(new Set(TOOLS.map((tool) => tool.name)).size).toBe(TOOLS.length)
  })

  /**
   * Bare verbs, not `shareRead`. A caller reaches these through a page it has
   * already navigated to, so a namespace prefix told it nothing it did not know
   * and was paid for on every listing. What a tool is about lives in its
   * description now, which is where a model reads it anyway.
   */
  test('names are bare camelCase verbs, with no namespace prefix', () => {
    for (const tool of TOOLS) {
      expect(tool.name, tool.name).toMatch(/^[a-z][A-Za-z]*$/)
      expect(tool.name.startsWith('share'), tool.name).toBe(false)
    }
  })
})

describe('descriptions', () => {
  test('every tool says what it does', () => {
    for (const tool of TOOLS) {
      expect(tool.description.length).toBeGreaterThan(20)
    }
  })

  // Descriptions are re-read on every tool listing, so their length is a
  // recurring context cost rather than a one-off.
  test('a description stays inside the 500 characters Chrome advises', () => {
    for (const tool of TOOLS) {
      expect(tool.description.length).toBeLessThanOrEqual(500)
    }
  })

  test('every parameter is described, in under 150 characters', () => {
    for (const tool of TOOLS) {
      for (const [key, property] of Object.entries(schemaOf(tool).properties)) {
        expect(property.description, `${tool.name}.${key}`).toBeTruthy()
        expect(property.description!.length, `${tool.name}.${key}`).toBeLessThanOrEqual(150)
      }
    }
  })
})

describe('input schemas', () => {
  test('each one is an object schema with properties', () => {
    for (const tool of TOOLS) {
      const schema = schemaOf(tool)
      expect(schema.type, tool.name).toBe('object')
      expect(typeof schema.properties, tool.name).toBe('object')
    }
  })

  test('required names a property that actually exists', () => {
    for (const tool of TOOLS) {
      const schema = schemaOf(tool)
      for (const name of schema.required ?? []) {
        expect(Object.keys(schema.properties), `${tool.name}.${name}`).toContain(name)
      }
    }
  })

  test('unknown properties are refused, so a typo is visible rather than ignored', () => {
    for (const tool of TOOLS) {
      expect(schemaOf(tool).additionalProperties, tool.name).toBe(false)
    }
  })

  test('every schema survives a JSON round trip, since the browser serializes it', () => {
    for (const tool of TOOLS) {
      expect(() => JSON.parse(JSON.stringify(tool.inputSchema))).not.toThrow()
    }
  })

  // Everything else defaults to the share already open in the tab, so an agent
  // can call it with no arguments at all.
  test('only the tools that cannot guess demand an argument', () => {
    const demanding = TOOLS.filter((tool) => (schemaOf(tool).required ?? []).length > 0)

    expect(demanding.map((tool) => tool.name).sort()).toEqual([
      'publish',
      'read',
      'search',
    ])
  })
})

describe('annotations', () => {
  test('every tool declares whether it changes anything', () => {
    for (const tool of TOOLS) {
      expect(typeof tool.annotations?.readOnlyHint, tool.name).toBe('boolean')
    }
  })

  /**
   * Reading a share changes nothing. Writing means bytes land somewhere they
   * were not — in this browser's storage, or in a share other peers can open.
   * Listed exhaustively so adding a tool has to make the choice knowingly.
   */
  test('every tool that changes something says so', () => {
    const writers = TOOLS.filter((tool) => tool.annotations?.readOnlyHint === false)

    expect(writers.map((tool) => tool.name).sort()).toEqual(['publish', 'sync'])
  })

  test('the read-only tools are the ones that only look', () => {
    const readers = TOOLS.filter((tool) => tool.annotations?.readOnlyHint === true)

    expect(readers.map((tool) => tool.name).sort()).toEqual([
      'connect',
      'list',
      'read',
      'search',
    ])
  })

  /**
   * A manifest is a remote peer's word for what it holds, and file bytes are
   * whatever that peer sent. Every tool that puts either in front of a model
   * has to say the content is not this origin's.
   */
  test('every tool that returns a peer’s bytes marks them untrusted', () => {
    const untrusted = TOOLS.filter((tool) => tool.annotations?.untrustedContentHint === true)

    expect(untrusted.map((tool) => tool.name).sort()).toEqual([
      'list',
      'read',
      'search',
    ])
  })
})

describe('registering', () => {
  // The common case, and the one that must stay free: no browser enables
  // WebMCP by default, so this runs on every page load and has to do nothing.
  test('a browser without WebMCP is a quiet no-op, not a failure', async () => {
    expect(document.modelContext).toBeUndefined()

    await expect(registerAgentTools()).resolves.toEqual({ registered: false, names: [] })
  })
})
