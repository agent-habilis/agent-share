# Vendored packages

Copied from the two visage-ui repos on 2026-08-06 (working trees, not git
HEAD — the moonspace package split and the visage-dom `this`-based context
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
7. `moonspace-dom/src/components/Button/` — forked back to the boxed
   presentation the app shipped with before the re-vendor (one-row chrome:
   1ch padding + inset transparent outline, per-variant fills, lowercase
   labels, fill-step hover, focus recolours the outline), replacing
   upstream's `[ brackets ]` style. Colors still come from the tokens.

## Workspace wiring (differs from upstream)

- `moonspace-dom/package.json` declares `visage-dom`/`visage-style` as
  `workspace:*` deps; upstream wires them via tsconfig `paths` to a sibling
  checkout instead. The `paths` blocks are removed here.
- The `visage-*` tsconfigs extend `web/tsconfig.base.json` (same options as
  upstream's base minus `types`), adding `types: ["bun"]` per package.
- Per-package `bunfig.toml` files preload `../../scripts/test-setup.ts`, which
  is `web/scripts/test-setup.ts` (identical to upstream's).
- The `visage-*` packages' `build` script is dropped. It ran upstream's
  per-package library build, which was never vendored, and these packages are
  consumed as source through their `exports`. Left in place it would have found
  `web/scripts/build.ts` — this app's bundler — and run that instead.
