# foliate-js (vendored)

- Upstream: https://github.com/johnfactotum/foliate-js
- Commit: `78914aef4466eb960965702401634c2cb348e9b1` (2026-05-01, "Use original hrefs for external links and add isExternal in fb2.js (#129)")
- License: MIT, `LICENSE` beside this file (Copyright (c) 2022 John Factotum)
- Not on npm as a package, hence vendored.

Files taken unchanged: `epub.js`, `epubcfi.js`, `paginator.js`, `progress.js`,
`overlayer.js`, `text-walker.js`, `search.js`, `fixed-layout.js`.

Taisce patches (each marked `TAISCE PATCH` in the code):

- `view.js`: `makeBook` opens EPUBs only (upstream also dispatches to the
  comic-book, FB2, PDF and MOBI loaders, which are not vendored); the format
  sniffers and directory loader those used are removed; `initTTS` is removed
  (`tts.js` is not vendored).
- `vendor/zip.js`: upstream ships a rollup bundle of `@zip.js/zip.js`; here it
  re-exports the npm package's no-worker, no-wasm build
  (`@zip.js/zip.js/lib/zip-core-custom.js`, BSD-3-Clause, kept at its latest in
  `ui/package.json`), so inflating runs on the browser's DecompressionStream.

To update: copy the same files from a newer upstream commit, re-apply the two
patches, record the commit here.
