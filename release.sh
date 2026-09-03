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

# Refs discipline: this checkout is shared with other sessions that may switch
# branches. A release is built from HEAD, so HEAD must be main and main must
# be what origin has — otherwise gh tags the wrong commit (it did, 0.6.2–0.6.4).
BRANCH=$(git branch --show-current)
if [[ "$BRANCH" != "main" ]]; then
  echo "✗ on branch '$BRANCH', not main — switch (or fast-forward main) before releasing"; exit 1
fi
if [[ -n "$(git status --short --untracked-files=no)" ]]; then
  echo "✗ uncommitted changes — commit first so the tag matches the build"; git status --short; exit 1
fi
git fetch -q origin
if [[ "$(git rev-parse HEAD)" != "$(git rev-parse origin/main)" ]]; then
  echo "✗ HEAD $(git rev-parse --short HEAD) != origin/main $(git rev-parse --short origin/main) — push first"; exit 1
fi
HEAD_SHA=$(git rev-parse HEAD)

echo "→ signing as: $IDENTITY"
export APPLE_SIGNING_IDENTITY="$IDENTITY"

# Updater artifacts (Grimoire.app.tar.gz + .sig) are minisign-signed with a
# key that lives OUTSIDE the repo; the matching pubkey is in tauri.conf.json.
# Generate once: ui/node_modules/.bin/tauri signer generate -w ~/.grimoire-release/updater.key
UPDATER_KEY="$HOME/.grimoire-release/updater.key"
if [[ ! -f "$UPDATER_KEY" ]]; then
  echo "✗ updater signing key missing at $UPDATER_KEY (see comment above)"
  exit 1
fi
export TAURI_SIGNING_PRIVATE_KEY="$(cat "$UPDATER_KEY")"
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD=""
VERSION=$(python3 -c 'import json;print(json.load(open("crates/shell/tauri.conf.json"))["version"])')

echo "→ embedding model"
./scripts/fetch-model.sh | tail -1
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

# The updater feed: one latest.json per release, pointing at the tar.gz asset
# of THIS release. The app checks releases/latest/download/latest.json, so the
# newest release's file is the live one.
TARBALL=$(ls -t target/release/bundle/macos/*.app.tar.gz | head -1)
SIG="$TARBALL.sig"
if [[ ! -f "$SIG" ]]; then
  echo "→ bundler did not sign the updater artifact; signing explicitly"
  ui/node_modules/.bin/tauri signer sign -f "$UPDATER_KEY" -p "" "$TARBALL" > /dev/null
fi
[[ -f "$SIG" ]] || { echo "✗ no updater signature next to $TARBALL"; exit 1 }
FEED=target/release/bundle/latest.json
python3 - "$VERSION" "$SIG" "$FEED" "$TARBALL" <<'PY'
import json, sys, datetime, os
version, sig, feed, tarball = sys.argv[1:]
name = os.path.basename(tarball)
json.dump({
  "version": version,
  "notes": f"Grimoire {version} — see the release page for details.",
  "pub_date": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
  "platforms": {
    "darwin-aarch64": {
      "signature": open(sig).read().strip(),
      "url": f"https://github.com/Nodstuff/grimoire/releases/download/v{version}/{name}",
    }
  },
}, open(feed, "w"), indent=2)
PY
echo "✓ dmg:       $DMG"
echo "✓ updater:   $TARBALL (+ .sig)"
echo "✓ feed:      $FEED"
echo "  sha256:    $(shasum -a 256 "$DMG" | cut -d' ' -f1)"

# --publish [notes.md]: create the GitHub release with all four assets as
# the personal account. Without it, upload by hand:
#   gh release create v$VERSION "$DMG" "$TARBALL" "$SIG" "$FEED" --repo Nodstuff/grimoire
if [[ "${1:-}" == "--publish" ]]; then
  NOTES="${2:-}"
  export GH_TOKEN=$(gh auth token --user Nodstuff)
  if [[ -n "$NOTES" ]]; then
    gh release create "v$VERSION" "$DMG" "$TARBALL" "$SIG" "$FEED" --repo Nodstuff/grimoire --title "v$VERSION" --notes-file "$NOTES" --target "$HEAD_SHA"
  else
    gh release create "v$VERSION" "$DMG" "$TARBALL" "$SIG" "$FEED" --repo Nodstuff/grimoire --title "v$VERSION" --generate-notes --target "$HEAD_SHA"
  fi
  echo "✓ published https://github.com/Nodstuff/grimoire/releases/tag/v$VERSION"
fi
