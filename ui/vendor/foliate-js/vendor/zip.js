// TAISCE PATCH: upstream vendors a rollup bundle of @zip.js/zip.js here.
// Taisce takes the npm package instead (ui/package.json, kept at its latest)
// and its no-worker, no-wasm build: inflate runs on the browser's native
// DecompressionStream, so the UI's CSP needs no worker-src and no wasm.
export { configure, ZipReader, BlobReader, TextWriter, BlobWriter } from '@zip.js/zip.js/lib/zip-core-custom.js'
