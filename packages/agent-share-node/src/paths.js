import { isAbsolute, resolve, sep } from 'node:path'

/**
 * Path hardening for manifest entries.
 *
 * `rel_path` is a remote peer's word for where a file belongs, and this CLI
 * writes real files. A manifest claiming `../../.ssh/authorized_keys` is the
 * obvious attack, and the obvious mistake is to check for `..` as a substring
 * and miss `a/../../b`, or to trust the string and let the OS resolve it.
 *
 * This is the JS counterpart of `safe_component` (`src/file/walk.rs`) and
 * `validate_rel_path` (`src/mount/nfs.rs`). Two checks, because either alone
 * has a hole: component-wise rejection catches traversal before any filesystem
 * call, and the containment check catches whatever the platform normalises
 * differently (case folding, trailing dots, alternate separators).
 */

/**
 * Components that can never appear in a safe relative path.
 * @param {string} component
 */
function isSafeComponent(component) {
  return (
    component !== '' &&
    component !== '.' &&
    component !== '..' &&
    !component.includes('\0') &&
    // Windows accepts `/` inside a component in some APIs; reject rather than
    // guess which layer normalises it.
    !component.includes('\\')
  )
}

/**
 * Resolve `relPath` under `root`, or throw.
 *
 * @param {string} root Absolute destination directory.
 * @param {string} relPath Manifest-supplied relative path.
 * @returns {string} The absolute path to write.
 */
export function safeJoin(root, relPath) {
  if (isAbsolute(relPath)) {
    throw new Error(`refusing an absolute path from the peer: ${relPath}`)
  }
  const parts = relPath.split('/')
  if (!parts.every(isSafeComponent)) {
    throw new Error(`refusing an unsafe path from the peer: ${relPath}`)
  }

  const target = resolve(root, ...parts)
  // Belt and braces: even with every component checked, compare the resolved
  // result against the root so a platform-specific normalisation cannot slip
  // past. The trailing separator stops `/tmp/share-evil` matching `/tmp/share`.
  const bounded = root.endsWith(sep) ? root : root + sep
  if (target !== root && !target.startsWith(bounded)) {
    throw new Error(`path escapes the destination: ${relPath}`)
  }
  return target
}
