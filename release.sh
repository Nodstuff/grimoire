#!/bin/zsh
# Signed, notarized, distributable Grimoire dmg (ADR 0002 distribution).
#
# One-time setup (account holder only):
#   1. Developer ID Application cert: Xcode → Settings → Accounts → Manage
#      Certificates → + → "Developer ID Application".
#   2. Notary credentials, stored once in the keychain:
#      xcrun notarytool store-credentials grimoire-notary \
#        --apple-id <apple-id-email> --team-id <TEAMID>
#      (prompts for an app-specific password from account.apple.com)
set -e
set -o pipefail
cd "$(dirname "$0")"

IDENTITY=$(security find-identity -v -p codesigning | rg -o '"Developer ID Application: [^"]+"' | head -1 | tr -d '"')
if [[ -z "$IDENTITY" ]]; then
  echo "✗ no 'Developer ID Application' certificate in the keychain."
  exit 1
fi
if ! xcrun notarytool history --keychain-profile grimoire-notary > /dev/null 2>&1; then
  echo "✗ notary credentials missing (see setup comment at the top of this script)"
  exit 1
fi

echo "→ signing as: $IDENTITY"
export APPLE_SIGNING_IDENTITY="$IDENTITY"

echo "→ ui build"
(cd ui && npm run build --silent | tail -1)
echo "→ daemon release build"
cargo build --release -p grimoire 2>&1 | tail -1
cp target/release/grimoire crates/shell/binaries/grimoire-aarch64-apple-darwin

echo "→ signed app + dmg"
# Tauri's dmg step drives Finder via AppleScript and occasionally flakes in
# non-interactive shells; one retry has always been enough.
(cd crates/shell && ../../ui/node_modules/.bin/tauri build --bundles app,dmg 2>&1 | rg "Finished|error" || true)
DMG=$(ls -t target/release/bundle/dmg/*.dmg 2>/dev/null | head -1) || true
if [[ -z "$DMG" ]]; then
  echo "→ dmg bundler flaked; retrying"
  (cd crates/shell && ../../ui/node_modules/.bin/tauri build --bundles dmg 2>&1 | rg "Finished|error" || true)
  DMG=$(ls -t target/release/bundle/dmg/*.dmg 2>/dev/null | head -1)
fi
[[ -n "$DMG" ]] || { echo "✗ no dmg produced"; exit 1 }

# Tauri signs but cannot use keychain notary profiles — notarize explicitly.
echo "→ notarizing $DMG (Apple's queue: usually 2-10 min)"
xcrun notarytool submit "$DMG" --keychain-profile grimoire-notary --wait
xcrun stapler staple "$DMG"

spctl -a -vv -t install "$DMG" 2>&1 | head -3 || true
echo "✓ shareable: $DMG"
