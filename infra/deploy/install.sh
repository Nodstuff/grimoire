#!/bin/bash
# Runs on the box via SSM. Pulls the release bundle from the backup bucket
# (the instance role can read it) and (re)installs binaries, configs and units.
set -euo pipefail
B=s3://taisce-backups-895102116452/releases/current
T=$(mktemp -d)
aws s3 cp --recursive --quiet "$B" "$T"
cd "$T"
sha256sum -c SHA256SUMS
install -m 0755 taisce portus-dataplane litestream apns-key.sh share-key.sh /opt/taisce/bin/
install -d -m 0755 /etc/portus
install -m 0644 portus.yaml /etc/portus/portus.yaml
install -m 0644 litestream.yml /etc/taisce/litestream.yml
install -m 0644 taisce.service portus.service litestream.service /etc/systemd/system/
# The daemon was called grimoire before 0.9: retire its unit and binary so
# the two never race for port 7425 or the db. A no-op once migrated.
if [[ -f /etc/systemd/system/grimoire.service ]]; then
  systemctl disable --now grimoire >/dev/null 2>&1 || true
  rm -f /etc/systemd/system/grimoire.service /opt/taisce/bin/grimoire
fi
systemctl daemon-reload
systemctl enable taisce portus litestream >/dev/null
systemctl restart taisce
ok=
for i in $(seq 1 30); do curl -sf -o /dev/null http://127.0.0.1:7425/healthz && ok=1 && break; sleep 1; done
[[ -n "$ok" ]] || { echo "taisce not healthy on 127.0.0.1:7425 after 30s"; journalctl -u taisce --no-pager -n 30; exit 1; }
systemctl restart portus litestream
rm -rf "$T"
systemctl --no-pager --lines=0 status taisce portus litestream | grep -E '●|Active:'
