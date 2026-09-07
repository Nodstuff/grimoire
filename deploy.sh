#!/bin/zsh
# Full deploy: UI dist + daemon sidecar + app bundle. One command, no drift.
# The daemon is the app's child (no launchd since 0.5): quitting the old app
# stops the old daemon, opening the new app starts the new one.
set -e
set -o pipefail
cd "$(dirname "$0")"
echo "→ embedding model"
./scripts/fetch-model.sh | tail -1
echo "→ ui build"
(cd ui && npm run build --silent | tail -1)
echo "→ daemon release build"
cargo build --release -p grimoire 2>&1 | tail -1
cp target/release/grimoire crates/shell/binaries/grimoire-aarch64-apple-darwin
echo "→ app bundle"
(cd crates/shell && ../../ui/node_modules/.bin/tauri build --bundles app 2>&1 | rg "Finished 1 bundle" || true)
osascript -e 'quit app "Grimoire"' 2>/dev/null; osascript -e 'quit app "knowledge-system"' 2>/dev/null || true
sleep 1; pkill -f grimoire-shell 2>/dev/null || true; sleep 0.5
rm -rf /Applications/knowledge-system.app /Applications/Grimoire.app
cp -R target/release/bundle/macos/Grimoire.app /Applications/
open /Applications/Grimoire.app
for i in $(seq 1 20); do curl -sf -o /dev/null http://127.0.0.1:7425/api/stamp && break; sleep 0.5; done
curl -s http://127.0.0.1:7425/api/stamp | rg -o '"version":"[^"]*"' || echo "  daemon not answering yet"
echo "✓ deployed: daemon, ui, app"
