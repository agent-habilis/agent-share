import { beforeEach } from 'bun:test'
import { GlobalRegistrator } from '@happy-dom/global-registrator'

GlobalRegistrator.register()

/**
 * happy-dom starts on `about:blank`, where `pushState` and `replaceState` are
 * rejected because there is no origin to be same-origin with. The router tests
 * need a real one; nothing else reads `location`, so setting it here costs the
 * other suites nothing.
 *
 * Per test, not once per process, because the whole run shares one document and
 * a *navigation* leaks across files: `visage-router`'s link tests follow an
 * `https://example.com` anchor, and every origin-absolute assertion scheduled
 * after them then reads that origin. `replaceState` cannot undo it — an origin
 * is not something history can rewrite — so the reset has to be `setURL`, and
 * it belongs here rather than in whichever file happened to notice, since the
 * document these tests share is exactly what this preload owns.
 */
declare const happyDOM: { setURL(url: string): void }

happyDOM.setURL('http://localhost/')
beforeEach(() => {
  happyDOM.setURL('http://localhost/')
})
