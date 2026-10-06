#!/bin/bash
# ExecStartPre (as root): restore the share-link key from SSM into
# /var/lib/taisce/share-links.key (0600, user taisce). Every link's URL is
# derived from it and Litestream copies only ks.db, so SSM is its backup: a
# rebuilt box gets the same key and every link keeps working. Failing here
# leaves the file as it is (the daemon makes one if there is none).
set -euo pipefail
umask 077
dest=/var/lib/taisce/share-links.key
tmp=$(mktemp /var/lib/taisce/.share-links.key.XXXXXX)
trap 'rm -f "$tmp"' EXIT
aws ssm get-parameter --region eu-west-1 --name /taisce/share-links/key --with-decryption \
  --query Parameter.Value --output text | base64 -d > "$tmp"
[ "$(stat -c %s "$tmp")" = 32 ] || { echo "share-key: SSM value is not 32 bytes" >&2; exit 1; }
if ! cmp -s "$tmp" "$dest" 2>/dev/null; then
  [ -e "$dest" ] && echo "share-key: replacing $dest with the SSM copy" >&2
  chown taisce:taisce "$tmp"
  mv "$tmp" "$dest"
fi
