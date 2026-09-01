#!/bin/zsh
# Signed, notarized, distributable Grimoire.dmg (ADR 0002 distribution).
#
# One-time setup (only the account holder can do these):
#   1. Developer ID Application cert: Xcode → Settings → Accounts → Manage
#      Certificates → + → "Developer ID Application" (then it appears in
#      `security find-identity -v -p codesigning`).
#   2. Notary credentials, stored once in the keychain:
#      xcrun notarytool store-credentials grimoire-notary \
#        --apple-id <apple-id-email> --team-id <TEAMID> \
#        --password <app-specific-password from appleid.apple.com>
#
# Tauri picks up APPLE_SIGNING_IDENTITY + the notary keychain profile and
# signs the app, the bundled sidecar daemon, and staples the ticket.
set -e
set -o pipefail
cd "$(dirname "$0")"

IDENTITY=$(security find-identity -v -p codesigning | rg -o '"Developer ID Application: [^"]+"' | head -1 | tr -d '"')
if [[ -z "$IDENTITY" ]]; then
  echo "✗ no 'Developer ID Application' certificate in the keychain."
  echo "  Create one: Xcode → Settings → Accounts → Manage Certificates → +"
  exit 1
fi
if ! xcrun notarytool history --keychain-profile grimoire-notary > /dev/null 2>&1; then
  echo "✗ notary credentials missing. Store them once with:"
  echo "  xcrun notarytool store-credentials grimoire-notary --apple-id <email> --team-id <TEAMID> --password <app-specific-password>"
  exit 1
fi

echo "→ signing as: $IDENTITY"
export APPLE_SIGNING_IDENTITY="$IDENTITY"
export APPLE_KEYCHAIN_PROFILE="grimoire-notary"

echo "→ ui build"
(cd ui && npm run build --silent | tail -1)
echo "→ daemon release build"
cargo build --release -p grimoire 2>&1 | tail -1
cp target/release/grimoire crates/shell/binaries/grimoire-aarch64-apple-darwin
echo "→ signed app + dmg build (incl. notarization — takes a few minutes)"
(cd crates/shell && ../../ui/node_modules/.bin/tauri build --bundles app,dmg 2>&1 | rg "Finished|Signing|Notariz|error" || true)

DMG=$(ls -t target/release/bundle/dmg/*.dmg 2>/dev/null | head -1)
if [[ -n "$DMG" ]]; then
  spctl -a -vv -t install "$DMG" 2>&1 | head -2 || true
  echo "✓ shareable: $DMG"
else
  echo "✗ no dmg produced — check the build output above"
  exit 1
fi
