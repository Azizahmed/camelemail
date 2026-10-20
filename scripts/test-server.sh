#!/usr/bin/env bash
# Start a throwaway GreenMail IMAP/SMTP server for Remail's integration test.
#   IMAP 3143 / IMAPS 3993, SMTP 3025 / SMTPS 3465 (self-signed). Any password works.
# Then run:  REMAIL_IT=1 cargo test -p remail-core --test greenmail
set -euo pipefail
name=remail-greenmail
if docker ps --format '{{.Names}}' | grep -qx "$name"; then
  echo "$name is already running"
  exit 0
fi
docker run -d --rm --name "$name" \
  -e GREENMAIL_OPTS='-Dgreenmail.setup.test.all -Dgreenmail.hostname=0.0.0.0 -Dgreenmail.users.login=email -Dgreenmail.auth.disabled' \
  -p 3025:3025 -p 3143:3143 -p 3465:3465 -p 3993:3993 \
  greenmail/standalone:2.1.3 >/dev/null
echo "GreenMail started ($name). Stop it with: docker stop $name"
