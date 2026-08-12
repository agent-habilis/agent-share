/**
 * Tool results, and the argument checking every tool has to do for itself.
 *
 * Two behaviours of the browser's WebMCP implementation shape this file, both
 * measured against Chrome 151 rather than read off the spec:
 *
 * 1. **Input is not validated against `inputSchema`.** A call that omits a
 *    `required` field still reaches `execute`, with that field `undefined`. The
 *    schema is documentation the agent reads, not a gate the browser enforces.
 * 2. **A throw is flattened.** Whatever an `execute` throws reaches the agent as
 *    `UnknownError: Tool was executed but the invocation failed`. The message is
 *    dropped, so a thrown error tells the agent nothing it can act on.
 *
 * Together those mean a tool must check its own arguments and must report
 * failure as an ordinary return value. `fail()` is not a fallback path here; it
 * is the only way to say anything useful about what went wrong.
 */

export interface ToolFailure {
  ok: false
  error: string
  /** Stable, machine-readable. The prose in `error` is for the model. */
  code: ToolErrorCode
}

export type ToolOk<T> = { ok: true } & T

export type ToolResult<T> = ToolOk<T> | ToolFailure

export type ToolErrorCode =
  | 'bad_argument'
  | 'no_session'
  | 'unauthorized'
  | 'not_found'
  | 'not_a_file'
  | 'not_a_directory'
  | 'unsupported'
  /** The browser refused because a real click is required. Not retryable. */
  | 'needs_user_gesture'
  | 'failed'

export function ok<T extends object>(data: T): ToolOk<T> {
  return { ok: true, ...data }
}

export function fail(code: ToolErrorCode, error: string): ToolFailure {
  return { ok: false, code, error }
}

/** Thrown inside a tool and converted by [`guard`]. Never escapes to the agent. */
export class ToolInputError extends Error {
  readonly code: ToolErrorCode

  constructor(code: ToolErrorCode, message: string) {
    super(message)
    this.name = 'ToolInputError'
    this.code = code
  }
}

/**
 * Run `body`, turning anything it throws into a failure result.
 *
 * The catch-all is deliberate. An unexpected throw from wasm or from a dead
 * connection would otherwise reach the agent as the generic browser error, and
 * "the invocation failed" is indistinguishable from a wrong password, a stale
 * path, or a share that went away.
 */
export async function guard<R extends ToolResult<object>>(
  body: () => Promise<R>,
): Promise<R | ToolFailure> {
  try {
    return await body()
  } catch (error) {
    if (error instanceof ToolInputError) return fail(error.code, error.message)
    return fail('failed', describe(error))
  }
}

/** A one-line rendering of an unknown throw, safe to hand to a model. */
export function describe(error: unknown): string {
  if (error instanceof Error) return error.message || error.name
  const text = String(error)
  return text.length > 300 ? `${text.slice(0, 300)}…` : text
}

function bad(message: string): never {
  throw new ToolInputError('bad_argument', message)
}

export function requiredString(input: Record<string, unknown>, key: string): string {
  const value = input[key]
  if (typeof value !== 'string' || value.trim() === '') {
    bad(`"${key}" is required and must be a non-empty string`)
  }
  return value.trim()
}

export function optionalString(
  input: Record<string, unknown>,
  key: string,
): string | undefined {
  const value = input[key]
  if (value === undefined || value === null) return undefined
  if (typeof value !== 'string') bad(`"${key}" must be a string`)
  const trimmed = value.trim()
  return trimmed === '' ? undefined : trimmed
}

export function optionalInt(
  input: Record<string, unknown>,
  key: string,
  { min, max, fallback }: { min: number; max: number; fallback: number },
): number {
  const value = input[key]
  if (value === undefined || value === null) return fallback
  const numeric = typeof value === 'string' ? Number(value) : value
  if (typeof numeric !== 'number' || !Number.isFinite(numeric)) {
    bad(`"${key}" must be a number`)
  }
  if (!Number.isInteger(numeric)) bad(`"${key}" must be a whole number`)
  if (numeric < min || numeric > max) bad(`"${key}" must be between ${min} and ${max}`)
  return numeric
}

export function optionalBool(
  input: Record<string, unknown>,
  key: string,
  fallback: boolean,
): boolean {
  const value = input[key]
  if (value === undefined || value === null) return fallback
  if (typeof value === 'boolean') return value
  if (value === 'true') return true
  if (value === 'false') return false
  bad(`"${key}" must be true or false`)
}

/**
 * A share-relative path, split into components and checked the way the tree
 * checks a manifest path.
 *
 * An empty path means the share root, which `shareList` and `shareStat` accept
 * and `shareRead` does not. Rejecting `..` here is belt-and-braces — nothing in
 * this module touches a filesystem — but a path that escapes the root can still
 * name a file the agent did not mean, and the manifest is a remote peer's word.
 */
export function sharePathParts(raw: string | undefined): string[] {
  if (raw === undefined) return []
  const trimmed = raw.replace(/^\/+/, '').replace(/\/+$/, '')
  if (trimmed === '' || trimmed === '.') return []
  const parts = trimmed.split('/')
  for (const part of parts) {
    if (part === '' || part === '.' || part === '..' || part.includes('\0')) {
      bad(`"${raw}" is not a valid path inside the share`)
    }
  }
  return parts
}
