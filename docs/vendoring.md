# Vendored packages

Six of the members of `packages/` are copies of upstream libraries, not code
written here. Nothing in the directory layout says so — they sit beside
`agent-share-web` and the rest as equals — so this file is the list, and each
one's `package.json` carries a `description` pointing back at it.

They were copied from the two visage-ui repos on 2026-08-06 (working trees, not
git HEAD — the moonspace package split and the visage-dom `this`-based context
API existed only as uncommitted changes at copy time):

- `visage-dom`, `visage-style`, `visage-router` ←
  `~/Developer/visage-ui/visage/packages/*` @ `82e7506` + dirty working tree
- `moonspace`, `moonspace-theme`, `moonspace-dom` ←
  `~/Developer/visage-ui/moonspace/packages/*` @ `dd5a773` + dirty working tree

Copied with `node_modules`, `dist`, and `.git` excluded. `moonspace-theme`'s
`stories/` directory (and its `./stories/*` export + `moonspace-dom` devDep)
was dropped — stories apps are not vendored.

## Local patches on top of upstream

Re-apply these when re-vendoring:

1. `visage-style/src/compile/index.ts` — `flex` kept unitless
   (`flex: 1`, not `flex: 1px`), plus test.
2. `visage-dom/src/dom/index.ts` — `UNITLESS` property set + `cssNumber()`
   so numeric inline styles like `opacity`/`flex` don't get `px` appended.
3. `visage-dom/src/jsx-runtime/index.ts` — plain stateless view functions
   usable directly in JSX (`<MyFn/>` without `component()`), incl. key
   hoisting.
4. `moonspace/src/theme/grid.ts` — row height forked to 22.5px
   (line-height 1.5) for the share browser's readability; upstream is 18px.
5. `moonspace-dom/src/components/ProgressBar/` — `fluid` prop (fill the
   container instead of a fixed cell width), plus test.
6. `moonspace-dom/src/components/MiddleTruncate/MiddleTruncate.tsx` —
   migrated to the `this`-based component context (upstream had not yet).
7. `visage-router/src/index.test.ts` — routers mounted by a test are torn
   down in an `afterEach`. Clearing `document.body` leaves the render root
   live, so a route's async generator runs on into the next test; measured,
   `a revisited lazy route…` failed ~3 runs in 8 under CPU saturation and ~1
   in 8 without it. The teardown takes that to ~1 in 12, so it is an
   improvement rather than a cure — see the feedback note in
   `~/Notes/projects/agent-share/feedback/`.
8. `moonspace-dom/src/components/Button/` — forked back to the boxed
   presentation the app shipped with before the re-vendor (one-row chrome:
   1ch padding + inset transparent outline, per-variant fills, lowercase
   labels, fill-step hover, focus recolours the outline), replacing
   upstream's `[ brackets ]` style. Colors still come from the tokens.

## Workspace wiring (differs from upstream)

- `moonspace-dom/package.json` declares `visage-dom`/`visage-style` as
  `workspace:*` deps; upstream wires them via tsconfig `paths` to a sibling
  checkout instead. The `paths` blocks are removed here.
- The `visage-*` tsconfigs extend `tsconfig.base.json` (same options as
  upstream's base minus `types`), adding `types: ["bun"]` per package.
  **Do not normalize `moonspace-dom` to extend the base as well.** It spells its
  `jsx`/`jsxImportSource` out inline, and that is load-bearing: Bun's bundler
  ignores `extends` for any tsconfig it reads through a `node_modules` symlink,
  so a package whose non-test `.tsx` is bundled that way has to state those two
  options itself or compile against `react/jsx-dev-runtime` and fail to
  resolve. `moonspace-dom` is the only member in that position — the first-party
  `.tsx` all lives in `agent-share-web`, which the bundler reaches by real path.
- Per-package `bunfig.toml` files preload `../../scripts/test-setup.ts`, which
  is `scripts/test-setup.ts` — upstream's, plus a `beforeEach` that resets the
  happy-dom URL. The whole run shares one document and `visage-router`'s link
  tests navigate away from `localhost`, so without the reset any origin-absolute
  assertion scheduled after them fails on run order alone.
- The `visage-*` packages' `build` script is dropped. It ran upstream's
  per-package library build, which was never vendored, and these packages are
  consumed as source through their `exports`. Left in place it would have found
  `scripts/build.ts` — this app's bundler — and run that instead.
