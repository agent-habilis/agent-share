/**
 * Ask for files with `<input type="file">`, the picker every browser has.
 *
 * This is how Safari and Firefox share. They implement no File System Access
 * picker, but an input gives back real `File`s, which the producer reads
 * lazily and by range exactly as it reads a handle. Nothing is copied.
 *
 * `webkitdirectory` turns the same input into a folder picker on desktop. iOS
 * Safari sets the property and then ignores it, so this module never asks
 * whether folder picking works — it reports what came back and lets the caller
 * see that the paths are flat.
 */

/** `folder` asks for a whole tree; `files` for a hand-picked set. */
export type PickMode = 'folder' | 'files'

/**
 * How long to wait after the window regains focus before calling it a cancel.
 *
 * The safety net for the `cancel` event, which Safari only got in 16.4. Long
 * enough that a `change` about to fire wins the race, short enough that a real
 * cancel does not look like a hang.
 */
const CANCEL_GRACE_MS = 500

/**
 * Open the picker and resolve with what was chosen.
 *
 * **Call this synchronously from a click handler.** `input.click()` spends the
 * transient user activation, and a single `await` above it turns the dialog
 * into a `NotAllowedError` — in Safari only, which no Chromium test run will
 * ever catch. Everything before the `.click()` below is deliberately
 * synchronous. Do not hoist an await past it.
 *
 * Rejects with an `AbortError` when the pick is cancelled, matching what
 * `showDirectoryPicker()` throws, so callers need one cancel path and not two.
 */
export function pickShareFiles(mode: PickMode): Promise<File[]> {
  const input = document.createElement('input')
  input.type = 'file'
  input.multiple = true
  if (mode === 'folder') input.webkitdirectory = true
  // Off-screen rather than hidden: WebKit has a long history of never firing
  // `change` on an input that is `display: none` or not in the document.
  input.style.cssText = 'position:fixed;left:-9999px;top:0;opacity:0;width:1px;height:1px'
  document.body.appendChild(input)

  const abort = new AbortController()
  const { signal } = abort

  const picked = new Promise<File[]>((resolve, reject) => {
    // Every path settles the same way: drop the listeners, drop the node.
    const settle = (): void => {
      abort.abort()
      input.remove()
    }
    const cancel = (): void => {
      settle()
      reject(new DOMException('The user aborted a request.', 'AbortError'))
    }

    input.addEventListener(
      'change',
      () => {
        const files = [...(input.files ?? [])]
        // An empty selection is a dialog dismissed, not a share of nothing.
        if (files.length === 0) {
          cancel()
          return
        }
        settle()
        resolve(files)
      },
      { signal },
    )
    input.addEventListener('cancel', cancel, { signal })

    // The net, for browsers older than the `cancel` event: a dialog that opened
    // took focus off the window, so focus coming back and no `change` following
    // it is a dismissal. Armed by the blur, never before — without that, a page
    // whose window never blurred would cancel its own live pick.
    let dialogTookFocus = false
    window.addEventListener('blur', () => {
      dialogTookFocus = true
    }, { signal })
    window.addEventListener(
      'focus',
      () => {
        if (!dialogTookFocus) return
        setTimeout(() => {
          if (!signal.aborted) cancel()
        }, CANCEL_GRACE_MS)
      },
      { signal },
    )
  })

  input.click()
  return picked
}
