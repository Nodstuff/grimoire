#!/bin/bash
# Runs on the box via SSM. Pulls the release bundle from the backup bucket
# (the instance role can read it) and (re)installs binaries, configs and units.
set -euo pipefail
B=s3://taisce-backups-895102116452/releases/current
T=$(mktemp -d)
aws s3 cp --recursive --quiet "$B" "$T"
cd "$T"
sha256sum -c SHA256SUMS
install -m 0755 grimoire portus-dataplane litestream apns-key.sh /opt/taisce/bin/
install -d -m 0755 /etc/portus
install -m 0644 portus.yaml /etc/portus/portus.yaml
install -m 0644 litestream.yml /etc/taisce/litestream.yml
install -m 0644 grimoire.service portus.service litestream.service /etc/systemd/system/
systemctl daemon-reload
systemctl enable grimoire portus litestream >/dev/null
systemctl restart grimoire
for i in $(seq 1 30); do curl -sf -o /dev/null http://127.0.0.1:7425/healthz && break; sleep 1; done
systemctl restart portus litestream
rm -rf "$T"
systemctl --no-pager --lines=0 status grimoire portus litestream | grep -E '●|Active:'
