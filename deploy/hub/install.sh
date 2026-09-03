#!/usr/bin/env bash
# Stand up a Grimoire hub on a fresh Debian/Ubuntu box (e.g. a small EC2 arm64
# instance). Builds from source — there is no Linux release artifact yet.
#
#   sudo ./install.sh "Team"          # hub name
#
# Afterwards, from the box:   grimoire --db /var/lib/grimoire/.grimoire/ks.db hub invite
# (the admin token lives at /var/lib/grimoire/.grimoire/admin.token; the CLI reads it)
set -euo pipefail
NAME="${1:-Team}"
REPO="${GRIMOIRE_REPO:-https://github.com/Nodstuff/grimoire.git}"
REF="${GRIMOIRE_REF:-main}"

apt-get update -qq && apt-get install -y -qq git curl build-essential pkg-config libssl-dev sqlite3 nodejs npm >/dev/null
id -u grimoire >/dev/null 2>&1 || useradd --system --home /var/lib/grimoire --create-home --shell /usr/sbin/nologin grimoire
install -d -o grimoire -g grimoire /var/lib/grimoire/.grimoire

if ! command -v cargo >/dev/null; then
  curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal >/dev/null
  # shellcheck disable=SC1091
  source "$HOME/.cargo/env"
fi
WORK=$(mktemp -d)
git clone -q --depth 1 --branch "$REF" "$REPO" "$WORK/grimoire"
cd "$WORK/grimoire"
(cd ui && npm ci --silent && npx vite build >/dev/null)
./scripts/fetch-model.sh | tail -1
cargo build --release -p grimoire 2>&1 | tail -1
install -m 0755 target/release/grimoire /usr/local/bin/grimoire

# name the hub once (persisted in the db); the unit's plain `serve --hub` keeps it
sudo -u grimoire timeout 8 /usr/local/bin/grimoire --db /var/lib/grimoire/.grimoire/ks.db --port 7425 serve --hub --name "$NAME" >/dev/null 2>&1 || true
install -m 0644 deploy/hub/grimoire-hub.service /etc/systemd/system/grimoire-hub.service
systemctl daemon-reload
systemctl enable --now grimoire-hub
sleep 3
echo "hub is up. First invite:"
sudo -u grimoire /usr/local/bin/grimoire --db /var/lib/grimoire/.grimoire/ks.db --port 7425 hub invite
