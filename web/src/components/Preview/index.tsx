/**
 * One file, rendered in place.
 *
 * Renders in the app content area, not a modal — the same treatment `TechInfo`
 * gets, and for the same reason: a preview is a place you navigate to, so it
 * has a URL and a back button rather than a dismiss.
 *
 * # Media streams; everything else buffers
 *
 * Video and audio go through the service worker, which answers `Range` with
 * 206 — so the element seeks on its own, and playback starts long before the
 * last byte lands. That is the only way a `<video src>` can seek at all: a Blob
 * URL needs every byte to exist before the URL does.
 *
 * When no worker is available — unsupported, an insecure context, private
 * browsing — media falls back to the same whole-file Blob that text and images
 * still use. That is an honest ceiling, and the answer is to say what is
 * happening rather than refuse: the loading state carries a spinner, a bar and
 * both byte counts, and leaving the view aborts the read mid-flight.
 *
 * Buffered reads go through `singleFileStream` rather than a loop of their own.
 * It already chunks at the protocol's 256 KiB ceiling, already carries the
 * short-read guard that tells a seeder's truncation apart from EOF, and already
 * reports progress — three things worth not writing twice. The worker path
 * carries that same guard, through `sourceIsOrigin` on the registration.
 */

import { MiddleTruncate, ProgressBar, Spinner, Stack, Text, t } from 'moonspace-dom'
import { component, listen, signal } from 'visage-dom'
import type { Child } from 'visage-dom'

import { singleFileStream, type Progress } from '../../lib/download/index.ts'
import { openStream, type Stream } from '../../lib/stream/index.ts'
import { mimeFor, previewKind } from './previewKind/index.ts'
import { humanBytes, type FileNode } from '../../lib/tree.ts'

export interface PreviewClient {
  read(index: number, offset: bigint, len: number): Promise<Uint8Array>
  readonly source_is_origin?: boolean
  /**
   * Keep what was read, so previewing a file also seeds it.
   *
   * Optional here only because the type is structural; the real client always
   * supplies it. `singleFileStream` does the calling — this view does not touch
   * bytes itself, which is exactly why hooking the reader rather than each
   * consumer was the right seam.
   */
  keep?(index: number, offset: bigint, bytes: Uint8Array): Promise<void>
}

export interface PreviewProps {
  /** Absent when the URL names no file, or names one the manifest lost. */
  node: FileNode | undefined
  client: PreviewClient
  /** Leave the preview. Bound to Escape as well as the chrome's Cancel. */
  onClose: () => void
}

/** Whether anything on the page is currently full-screen. */
function inFullscreen(): boolean {
  const doc = document as Document & { webkitFullscreenElement?: Element | null }
  return Boolean(document.fullscreenElement ?? doc.webkitFullscreenElement)
}

/**
 * The pane itself, and the thing that holds keyboard focus while it is open.
 *
 * `tabIndex={-1}` plus the focus on mount is not decoration. A key press only
 * reaches a page through whatever has focus, and the button that opened this
 * view was unmounted by the very navigation that opened it — so focus fell
 * back to `<body>`, which is exactly the state where a browser is most willing
 * to keep Escape for itself. Focusing the pane also gives the text preview
 * arrow-key scrolling, which a `<div>` nobody can focus does not have.
 *
 * `onkeydown` here as well as the window listener, for the same reason:
 * whichever route the key takes, one of the two is on it. Handling it twice is
 * harmless — the first call navigates, and the second finds `defaultPrevented`
 * already set.
 */
function Pane({
  onkeydown,
  children,
}: {
  onkeydown: (event: Event) => void
  children: Child
}) {
  return (
    <div
      tabIndex={-1}
      onkeydown={onkeydown}
      ref={(el) => {
        ;(el as HTMLElement | null)?.focus({ preventScroll: true })
      }}
      style={{
        flex: 1,
        minHeight: 0,
        display: 'flex',
        flexDirection: 'column',
        // The focus is a plumbing detail, not something to draw a ring around:
        // the user asked to look at a file, not to select a region.
        outline: 'none',
      }}
    >
      {children}
    </div>
  )
}

/** The content area, with one thing in the middle of it. */
function Centered({ children }: { children: Child }) {
  return (
    <div
      style={{
        flex: 1,
        minHeight: 0,
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
        padding: '0 2ch',
      }}
    >
      {children}
    </div>
  )
}

/**
 * Name, size and date — the same three lines the file browser's detail pane
 * draws, because this is the fallback for a file the app cannot show and the
 * detail pane is already the answer to "what is this file".
 */
function FileFacts({ node }: { node: FileNode }) {
  return (
    <>
      <Text weight="bold">
        <MiddleTruncate value={node.name} />
      </Text>
      <Text color="fgMuted">{humanBytes(node.size)}</Text>
      {node.mtime > 0 ? (
        <Text color="fgSubtle">{new Date(node.mtime * 1000).toISOString().slice(0, 10)}</Text>
      ) : null}
    </>
  )
}

export const Preview = component<PreviewProps>(function* (props) {
  const ctx = this
  const node = props.node
  const kind = node ? previewKind(node.name) : 'unsupported'

  /** Decoded text, for the `text` kind. */
  const body = signal<string | null>(null)
  /**
   * What the element's `src` points at: a stream URL when the worker took the
   * file, an object URL when it was buffered. Revoking is unconditional on the
   * way out — it is a no-op on anything that is not an object URL, and the
   * alternative is remembering which kind this is in a second place.
   */
  const url = signal<string | null>(null)
  const progress = signal<Progress>({ done: 0, total: node?.size ?? 0 })
  const error = signal<string | null>(null)
  /**
   * The element refused the bytes. Container and codec support vary per
   * browser — `mov` and `flac` are the usual casualties — and a black
   * rectangle that never plays is the one outcome worth naming out loud.
   */
  const unplayable = signal(false)

  /** The registered stream, while one is up. Taken down on the way out. */
  let streaming: Stream | null = null

  const abort = new AbortController()
  ctx.aborted.addEventListener('abort', () => {
    abort.abort()
    const current = url.peek()
    if (current) URL.revokeObjectURL(current)
    streaming?.release()
    streaming = null
  })

  async function load(file: FileNode): Promise<void> {
    const onProgress = (next: Progress) => {
      progress.value = next
    }
    try {
      // Media first tries the service worker, which is the only way a
      // `<video src>` can seek: the element issues `Range` requests and the
      // worker answers 206, so playback starts before the file is complete and
      // scrubbing does not restart the download. `null` means no worker is
      // available — no support, an insecure context, private browsing — and the
      // whole-file Blob below is the honest fallback, exactly as before.
      if (kind === 'video' || kind === 'audio') {
        streaming = await openStream(
          props.client,
          { index: file.index, size: file.size, mime: mimeFor(file.name) },
          file.name,
        )
        if (abort.signal.aborted) {
          streaming?.release()
          streaming = null
          return
        }
        if (streaming) {
          // No progress to report: the element decides what to fetch and when,
          // so there is no total to count against.
          url.value = streaming.url
          return
        }
      }
      const stream = singleFileStream(props.client, file, onProgress, abort.signal)
      if (kind === 'text') {
        const text = await new Response(stream).text()
        if (abort.signal.aborted) return
        body.value = text
        return
      }
      const bytes = await new Response(stream).arrayBuffer()
      // Aborting after the bytes land but before the URL exists would otherwise
      // leak one: the unmount listener has already run and has nothing to
      // revoke.
      if (abort.signal.aborted) return
      url.value = URL.createObjectURL(new Blob([bytes], { type: mimeFor(file.name) }))
    } catch (err) {
      // Leaving the view aborts the read, which is a decision rather than a
      // failure — and the component is on its way out, so there is nobody left
      // to tell.
      if (abort.signal.aborted) return
      error.value = err instanceof Error ? err.message : String(err)
    }
  }

  if (node && kind !== 'unsupported') void load(node)

  function onEscape(event: Event): void {
    const key = event as KeyboardEvent
    if (key.key !== 'Escape' || key.defaultPrevented || inFullscreen()) return
    key.preventDefault()
    props.onClose()
  }

  /*
    Escape leaves, the way it does in the Info pane.

    Capture phase, and on `window`: that is the earliest point the page can see
    a key, ahead of anything a media element's controls might swallow. A
    `<video>` with focus handles several keys itself, and the bubble phase is
    downstream of that.

    Not while something is full-screen, though: Escape is already how you leave
    a full-screen video, and the browser's own handling arrives alongside this
    one — so without the guard a single press would both restore the window and
    navigate out from under it, and the video the user was watching would be
    gone in one keystroke.
  */
  using _keys = listen(window, 'keydown', onEscape, { capture: true })

  yield () => {
    // Read up front, unconditionally: a branch that reads no signal is a
    // render nothing can ever wake, and the framework says so out loud.
    const failed = error.value
    const cannotPlay = unplayable.value
    const loaded = kind === 'text' ? body.value !== null : url.value !== null

    function content(): Child {
      if (!node) {
        return (
          <Centered>
            <Text color="fgSubtle">nothing to preview</Text>
          </Centered>
        )
      }

      if (failed) {
        return (
          <Centered>
            <Stack direction="column" gap={1} align="center">
              <FileFacts node={node} />
              <Text color="danger" class="selectable">
                {failed}
              </Text>
            </Stack>
          </Centered>
        )
      }

      if (kind === 'unsupported' || cannotPlay) {
        return (
          <Centered>
            <Stack direction="column" gap={1} align="center">
              <FileFacts node={node} />
              <Text color="fgSubtle">
                {cannotPlay
                  ? 'this browser cannot play this file'
                  : 'no preview for this file type'}
              </Text>
            </Stack>
          </Centered>
        )
      }

      if (!loaded) {
        const { done, total } = progress.value
        return (
          <Centered>
            <Stack direction="column" gap={1} align="center">
              <Text weight="bold">
                <MiddleTruncate value={node.name} />
              </Text>
              <Stack direction="row" gap={1}>
                <Spinner />
                <Text color="fgMuted">
                  {humanBytes(done)} / {humanBytes(total)}
                </Text>
              </Stack>
              <ProgressBar
                width={40}
                showValue
                value={total === 0 ? 0 : done / total}
                label={`Loading ${node.name}`}
              />
            </Stack>
          </Centered>
        )
      }

      if (kind === 'text') {
        return (
          <div
            class="selectable"
            style={{
              flex: 1,
              minHeight: 0,
              overflow: 'auto',
              padding: '0 2ch',
              // The file decides its own line breaks; wrapping the rest keeps a
              // long line readable without a horizontal scrollbar under it.
              whiteSpace: 'pre-wrap',
              overflowWrap: 'anywhere',
              color: t.fg,
            }}
          >
            {body.value}
          </div>
        )
      }

      const src = url.value ?? ''
      const onerror = () => {
        unplayable.value = true
      }
      if (kind === 'image') {
        return (
          <Centered>
            <img
              src={src}
              alt={node.name}
              onerror={onerror}
              style={{ maxWidth: '100%', maxHeight: '100%', objectFit: 'contain' }}
            />
          </Centered>
        )
      }
      if (kind === 'video') {
        return (
          <Centered>
            <video
              src={src}
              controls
              onerror={onerror}
              style={{ maxWidth: '100%', maxHeight: '100%' }}
            />
          </Centered>
        )
      }
      return (
        <Centered>
          <Stack direction="column" gap={1} align="center">
            <Text weight="bold">
              <MiddleTruncate value={node.name} />
            </Text>
            <audio src={src} controls onerror={onerror} style={{ maxWidth: '100%' }} />
          </Stack>
        </Centered>
      )
    }

    return <Pane onkeydown={onEscape}>{content()}</Pane>
  }
})
