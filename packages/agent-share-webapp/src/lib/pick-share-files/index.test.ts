/**
 * The picker's whole job is to settle exactly once and leave nothing behind.
 * Both matter: a pick that never settles leaves the landing page looking dead,
 * and an input left in the document accumulates one node per attempt.
 */

import { test, expect, beforeEach } from 'bun:test'

import { pickShareFiles } from './index.ts'

beforeEach(() => {
  document.body.innerHTML = ''
})

function openInput(): HTMLInputElement {
  const input = document.body.querySelector('input[type=file]')
  if (!input) throw new Error('the picker put no input in the document')
  return input as HTMLInputElement
}

/** `input.files` is read-only, so a test supplies its own list. */
function choose(input: HTMLInputElement, files: File[]): void {
  Object.defineProperty(input, 'files', { value: files, configurable: true })
  input.dispatchEvent(new Event('change'))
}

/**
 * Open a picker, read the input while the dialog is notionally up, and always
 * settle the promise — a test that leaves one pending leaves its window
 * listeners armed, and the next test's events reach them.
 */
async function inspect(
  mode: 'folder' | 'files',
): Promise<{ input: HTMLInputElement; connectedWhileOpen: boolean }> {
  const picked = pickShareFiles(mode)
  const input = openInput()
  const connectedWhileOpen = input.isConnected
  input.dispatchEvent(new Event('cancel'))
  await picked.catch(() => {})
  return { input, connectedWhileOpen }
}

test('asks for a directory only in folder mode', async () => {
  // happy-dom leaves the property undefined until it is set, so this is a
  // was-it-turned-on check rather than a strict `false`.
  expect((await inspect('folder')).input.webkitdirectory).toBe(true)
  expect((await inspect('files')).input.webkitdirectory).not.toBe(true)
})

test('resolves with what was chosen and removes the input', async () => {
  const picked = pickShareFiles('files')
  const file = new File(['hi'], 'a.txt')
  choose(openInput(), [file])
  await expect(picked).resolves.toEqual([file])
  expect(document.body.querySelector('input[type=file]')).toBeNull()
})

test('a cancelled pick rejects as AbortError and removes the input', async () => {
  const picked = pickShareFiles('folder')
  openInput().dispatchEvent(new Event('cancel'))
  await expect(picked).rejects.toMatchObject({ name: 'AbortError' })
  expect(document.body.querySelector('input[type=file]')).toBeNull()
})

test('an empty selection is a dismissal, not a share of nothing', async () => {
  const picked = pickShareFiles('files')
  choose(openInput(), [])
  await expect(picked).rejects.toMatchObject({ name: 'AbortError' })
})

test('the input is in the document and not display:none', async () => {
  // WebKit has a long history of never firing `change` on an input that is
  // hidden or detached, so this is the reason the element is placed off-screen.
  const { input, connectedWhileOpen } = await inspect('files')
  expect(connectedWhileOpen).toBe(true)
  expect(input.style.display).not.toBe('none')
  expect(input.style.position).toBe('fixed')
})

test('window focus alone never cancels a live pick', async () => {
  // The net for browsers without a `cancel` event is armed by the blur that
  // opening a dialog causes. Without that arming, a page that merely regained
  // focus would cancel a pick the user is still making.
  const picked = pickShareFiles('files')
  window.dispatchEvent(new Event('focus'))
  await new Promise((resolve) => setTimeout(resolve, 600))
  const file = new File(['hi'], 'a.txt')
  choose(openInput(), [file])
  await expect(picked).resolves.toEqual([file])
})

test('focus after a blur, with no change following, is a cancel', async () => {
  const picked = pickShareFiles('files')
  window.dispatchEvent(new Event('blur'))
  window.dispatchEvent(new Event('focus'))
  await expect(picked).rejects.toMatchObject({ name: 'AbortError' })
  expect(document.body.querySelector('input[type=file]')).toBeNull()
})
