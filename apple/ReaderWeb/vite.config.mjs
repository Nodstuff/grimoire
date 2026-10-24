// Builds the app's reader page script (apple/scripts/build-reader.sh).

import { fileURLToPath } from 'node:url'

const here = fileURLToPath(new URL('.', import.meta.url))

export default ({
  root: here,
  logLevel: 'warn',
  build: {
    outDir: fileURLToPath(new URL('../App/Taisce/Resources/reader/', import.meta.url)),
    emptyOutDir: false,
    minify: true,
    target: 'safari18',
    lib: {
      entry: fileURLToPath(new URL('main.ts', import.meta.url)),
      formats: ['iife'],
      name: 'TaisceReader',
      fileName: () => 'reader.js',
    },
  },
})
