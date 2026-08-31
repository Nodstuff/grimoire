#!/bin/zsh
# Full deploy: UI dist + daemon (launchd) + app bundle w/ sidecar. One command, no drift.
set -e
set -o pipefail
cd "$(dirname "$0")"
echo "→ ui build"
(cd ui && npm run build --silent | tail -1)
echo "→ daemon release build"
cargo build --release -p ksd 2>&1 | tail -1
cp target/release/ksd crates/shell/binaries/ksd-aarch64-apple-darwin
echo "→ reload launchd daemon"
launchctl unload ~/Library/LaunchAgents/ie.null.knowledge-system.plist 2>/dev/null || true
launchctl load ~/Library/LaunchAgents/ie.null.knowledge-system.plist
sleep 1
curl -sf -o /dev/null http://127.0.0.1:7425/api/docs && echo "  daemon up"
echo "→ app bundle"
(cd crates/shell && ../../ui/node_modules/.bin/tauri build 2>&1 | rg "Finished 1 bundle" || true)
osascript -e 'quit app "Grimoire"' 2>/dev/null; osascript -e 'quit app "knowledge-system"' 2>/dev/null || true
sleep 1; pkill -f ks-shell 2>/dev/null || true; sleep 0.5
rm -rf /Applications/knowledge-system.app /Applications/Grimoire.app
cp -R target/release/bundle/macos/Grimoire.app /Applications/
open /Applications/Grimoire.app
echo "✓ deployed: daemon, ui, app"
