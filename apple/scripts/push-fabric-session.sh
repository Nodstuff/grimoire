#!/usr/bin/env bash
# Deliver the server's session notification (crates/daemon/src/push.rs,
# Push::FabricSession) to a booted simulator every 6 seconds until killed (slower than a banner
# lasts, so they never queue up), for
# FleetUITests.testASessionNotification… (a test runner cannot push to its own
# simulator). The ids are FleetStubServer's first session.
#
# Usage: apple/scripts/push-fabric-session.sh <simulator udid> [bundle id]
set -euo pipefail
DEVICE="${1:?simulator udid}"
BUNDLE="${2:-ie.null.taisce}"
PAYLOAD="$(mktemp "${TMPDIR:-/tmp}/fabric-push.XXXXXX")"
trap 'rm -f "$PAYLOAD"' EXIT
cat > "$PAYLOAD" <<'JSON'
{"aps":{"alert":{"title":"taisce/main","body":"Needs you"},"sound":"default","thread-id":"fabric:0196f3a0-0000-7000-8000-000000000001"},
 "kind":"fabric_session","session_id":"0196f3a0-0000-7000-8000-000000000001","runtime_id":"01a1fb15-5eea-78d0-a6b7-adbb1135747c","state":"needs_you"}
JSON
while true; do
  xcrun simctl push "$DEVICE" "$BUNDLE" "$PAYLOAD" >/dev/null 2>&1 || true
  sleep 6
done
