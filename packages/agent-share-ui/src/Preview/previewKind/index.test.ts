import { describe, expect, test } from 'bun:test'

import { mimeFor, previewKind } from './index.ts'

describe('previewKind', () => {
  test('text is deliberately narrow', () => {
    expect(previewKind('notes.txt')).toBe('text')
    expect(previewKind('README.md')).toBe('text')
    expect(previewKind('README.markdown')).toBe('text')
    // Plausibly text, but rendering a guess as replacement characters is worse
    // than saying there is no preview.
    expect(previewKind('server.log')).toBe('unsupported')
    expect(previewKind('data.json')).toBe('unsupported')
  })

  test('images, video and audio', () => {
    expect(previewKind('photo.png')).toBe('image')
    expect(previewKind('photo.jpeg')).toBe('image')
    expect(previewKind('logo.svg')).toBe('image')
    expect(previewKind('clip.mp4')).toBe('video')
    expect(previewKind('clip.webm')).toBe('video')
    expect(previewKind('song.mp3')).toBe('audio')
    expect(previewKind('song.flac')).toBe('audio')
  })

  test('the extension is matched case-insensitively', () => {
    expect(previewKind('PHOTO.PNG')).toBe('image')
    expect(previewKind('Notes.Txt')).toBe('text')
  })

  test('a name with no extension has nothing to go on', () => {
    expect(previewKind('LICENSE')).toBe('unsupported')
    expect(previewKind('')).toBe('unsupported')
  })

  test('a leading dot starts a name, not an extension', () => {
    expect(previewKind('.gitignore')).toBe('unsupported')
    expect(previewKind('.txt')).toBe('unsupported')
  })

  test('only the last extension counts', () => {
    expect(previewKind('archive.tar.gz')).toBe('unsupported')
    expect(previewKind('report.draft.md')).toBe('text')
  })

  test('a trailing dot is not an extension', () => {
    expect(previewKind('weird.')).toBe('unsupported')
  })
})

describe('mimeFor', () => {
  test('media carries the type its element needs to play it', () => {
    expect(mimeFor('clip.mp4')).toBe('video/mp4')
    expect(mimeFor('song.mp3')).toBe('audio/mpeg')
    expect(mimeFor('photo.png')).toBe('image/png')
  })

  test('the aliases resolve to one canonical type', () => {
    expect(mimeFor('a.jpg')).toBe(mimeFor('b.jpeg'))
    expect(mimeFor('a.m4v')).toBe(mimeFor('b.mp4'))
    expect(mimeFor('a.oga')).toBe(mimeFor('b.ogg'))
  })

  test('an unknown name yields no type rather than a guess', () => {
    expect(mimeFor('big.bin')).toBe('')
    expect(mimeFor('LICENSE')).toBe('')
  })

  test('every previewable kind has a type', () => {
    for (const name of ['a.txt', 'a.md', 'a.png', 'a.svg', 'a.webm', 'a.opus']) {
      expect(previewKind(name)).not.toBe('unsupported')
      expect(mimeFor(name)).not.toBe('')
    }
  })
})
