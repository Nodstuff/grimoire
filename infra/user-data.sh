#!/bin/bash
# First boot only. Binaries (taisce daemon, portus, litestream) and their
# systemd units are shipped later over SSM; this just prepares the box.
set -euo pipefail

dnf -y upgrade --refresh

# Portus runs natively (not in its container), so its ACME cache only has to be
# writable by the user it runs as; the "uid 1000" rule in docs/standalone.md is
# the container image's user.
id portus >/dev/null 2>&1 || useradd -r -s /sbin/nologin -d /var/lib/portus portus
id taisce >/dev/null 2>&1 || useradd -r -s /sbin/nologin -d /var/lib/taisce taisce

# Persistent state on the root EBS volume (delete_on_termination = false).
install -d -o portus -g portus -m 0700 /var/lib/portus /var/lib/portus/acme
install -d -o taisce -g taisce -m 0700 /var/lib/taisce
install -d -m 0755 /opt/taisce/bin /etc/taisce
