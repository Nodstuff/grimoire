#!/bin/bash
# ExecStartPre (as root): fetch the APNs auth key from SSM into a 0600 file the
# daemon (user taisce) reads. Failing here leaves push off; the daemon still starts.
set -euo pipefail
# /run/taisce is the unit's RuntimeDirectory (created by systemd, owned by taisce)
umask 077
aws ssm get-parameter --region eu-west-1 --name /taisce/apns/key --with-decryption \
  --query Parameter.Value --output text > /run/taisce/apns.p8.tmp
mv /run/taisce/apns.p8.tmp /run/taisce/apns.p8
chown taisce:taisce /run/taisce/apns.p8
