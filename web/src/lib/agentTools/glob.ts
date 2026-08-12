/**
 * A small glob matcher, so `shareSearch` can be pointed at part of a share.
 *
 * Written here rather than pulled in: the app has no glob dependency, the
 * grammar needed is four characters wide, and a matcher run against a remote
 * peer's path list is somewhere a regex built from user input should be
 * inspectable.
 *
 * A pattern with no `/` matches the file's name; a pattern with one matches the
 * whole share-relative path. That is the convention `rg -g` uses, and it makes
 * the common case — `*.rs` — mean what it looks like it means.
 */

/** Escape everything a regex treats specially, except the glob metacharacters. */
function escapeLiteral(text: string): string {
  return text.replace(/[.+^${}()|[\]\\]/g, '\\$&')
}

export function globToRegExp(pattern: string): RegExp {
  let source = ''
  let index = 0
  while (index < pattern.length) {
    const char = pattern[index]
    if (char === '*') {
      // `**` crosses directory separators; a single `*` stops at one.
      if (pattern[index + 1] === '*') {
        source += '.*'
        index += 2
        // `**/` should also match zero directories, so `**/x` finds a bare `x`.
        if (pattern[index] === '/') index += 1
        continue
      }
      source += '[^/]*'
      index += 1
      continue
    }
    if (char === '?') {
      source += '[^/]'
      index += 1
      continue
    }
    source += escapeLiteral(char as string)
    index += 1
  }
  return new RegExp(`^${source}$`)
}

export interface PathFilter {
  (path: string): boolean
}

export function pathFilter(pattern: string | undefined): PathFilter {
  if (!pattern) return () => true
  const matchesFullPath = pattern.includes('/')
  const regex = globToRegExp(pattern)
  return (path: string) => {
    if (matchesFullPath) return regex.test(path)
    const name = path.slice(path.lastIndexOf('/') + 1)
    return regex.test(name)
  }
}
