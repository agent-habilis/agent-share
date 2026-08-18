/**
 * Where the app's three build entrypoints live, spelled once.
 *
 * `build.ts` and `dev.ts` both bundle the service worker and would otherwise
 * repeat the same long path; a rename that missed one would break only the dev
 * server or only the production bundle, and a stale `sw.js` surfaces far from
 * here — as a decode error on a range request.
 *
 * `dev.ts` still imports the two HTML files with static specifiers rather than
 * these constants: Bun's HTML bundler needs a literal to follow at parse time.
 * So the pages are shared with `build.ts` in spirit only, and these two exist
 * for it alone.
 */

export const APP_HTML = './packages/agent-share-app/src/index.html'
export const LAB_HTML = './packages/agent-share-app/src/lab/index.html'
export const SW_ENTRY = './packages/agent-share-app/src/service-worker/index.ts'
