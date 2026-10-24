#!/bin/sh
# Rebuild the in-app reader's script (apple/App/Taisce/Resources/reader/reader.js)
# from apple/ReaderWeb/main.ts, the vendored foliate-js (ui/vendor/foliate-js,
# 78914ae) and ui/src/reader/xpointer.ts, with the ui's own vite and
# @zip.js/zip.js (cd ui && npm ci first). Commit the output and its SHA256SUMS.
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root/ui"
node_modules/.bin/vite build --config "$root/apple/ReaderWeb/vite.config.mjs"
cd "$root/apple/App/Taisce/Resources/reader"
shasum -a 256 reader.js reader.html > SHA256SUMS
echo "built $(wc -c < reader.js | tr -d ' ') bytes"
