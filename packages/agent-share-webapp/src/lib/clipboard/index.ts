/**
 * Copy `text` to the clipboard. Returns whether it worked.
 *
 * `navigator.clipboard` only exists on secure contexts, and Safari leaves it
 * `undefined` rather than throwing on use. A plain-http origin such as the
 * LAN or Tailscale dev URL therefore needs the old selection-based path.
 */
export async function copyText(text: string): Promise<boolean> {
  if (navigator.clipboard?.writeText) {
    await navigator.clipboard.writeText(text)
    return true
  }
  const area = document.createElement('textarea')
  area.value = text
  area.readOnly = true
  area.style.position = 'fixed'
  area.style.opacity = '0'
  document.body.append(area)
  area.select()
  try {
    return document.execCommand('copy')
  } finally {
    area.remove()
  }
}
