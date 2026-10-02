/**
 * What a file can be shown as, decided from its name.
 *
 * The extension is the only signal there is. The wire manifest carries
 * `{rel_path, size, mode, mtime}` and nothing else — no content type, by
 * design — and sniffing magic bytes would mean a round trip to the peer before
 * the page could decide what to draw. A name that lies gets the wrong element,
 * which fails loudly in the element's own error handler rather than silently.
 *
 * `mime` is not decoration: a Blob built without a type will not play in
 * `<video>` or `<audio>`, so the table below is what makes media previewable
 * at all.
 *
 * One table, one place to extend. Text stays deliberately narrow — a preview
 * that renders a `.bin` as replacement characters is worse than one that says
 * it cannot.
 */

export type PreviewKind = 'text' | 'image' | 'video' | 'audio' | 'unsupported'

interface Entry {
  kind: PreviewKind
  mime: string
}

const TYPES: Record<string, Entry> = {
  txt: { kind: 'text', mime: 'text/plain' },
  md: { kind: 'text', mime: 'text/markdown' },
  markdown: { kind: 'text', mime: 'text/markdown' },

  png: { kind: 'image', mime: 'image/png' },
  jpg: { kind: 'image', mime: 'image/jpeg' },
  jpeg: { kind: 'image', mime: 'image/jpeg' },
  gif: { kind: 'image', mime: 'image/gif' },
  webp: { kind: 'image', mime: 'image/webp' },
  avif: { kind: 'image', mime: 'image/avif' },
  bmp: { kind: 'image', mime: 'image/bmp' },
  ico: { kind: 'image', mime: 'image/x-icon' },
  // Safe in an `<img>`: that context runs no script, however the document was
  // authored. Anywhere it could — an `<object>`, an iframe, `innerHTML` — it
  // would be running a stranger's markup on this origin.
  svg: { kind: 'image', mime: 'image/svg+xml' },

  mp4: { kind: 'video', mime: 'video/mp4' },
  m4v: { kind: 'video', mime: 'video/mp4' },
  webm: { kind: 'video', mime: 'video/webm' },
  ogv: { kind: 'video', mime: 'video/ogg' },
  mov: { kind: 'video', mime: 'video/quicktime' },

  mp3: { kind: 'audio', mime: 'audio/mpeg' },
  wav: { kind: 'audio', mime: 'audio/wav' },
  ogg: { kind: 'audio', mime: 'audio/ogg' },
  oga: { kind: 'audio', mime: 'audio/ogg' },
  opus: { kind: 'audio', mime: 'audio/ogg' },
  m4a: { kind: 'audio', mime: 'audio/mp4' },
  aac: { kind: 'audio', mime: 'audio/aac' },
  flac: { kind: 'audio', mime: 'audio/flac' },
}

/**
 * The part after the last dot, lowercased.
 *
 * A leading dot does not start an extension — `.gitignore` is a name, not a
 * suffix — and `archive.tar.gz` is `gz`, which the table does not know, so
 * both land on `unsupported` rather than on a guess.
 */
function extension(name: string): string {
  const dot = name.lastIndexOf('.')
  if (dot <= 0) return ''
  return name.slice(dot + 1).toLowerCase()
}

function entry(name: string): Entry | undefined {
  const ext = extension(name)
  return ext ? TYPES[ext] : undefined
}

/** How to render this file, or `'unsupported'` when there is no answer. */
export function previewKind(name: string): PreviewKind {
  return entry(name)?.kind ?? 'unsupported'
}

/** Content type for the preview Blob. Empty when the name says nothing. */
export function mimeFor(name: string): string {
  return entry(name)?.mime ?? ''
}
