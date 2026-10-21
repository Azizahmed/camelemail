#!/bin/sh
# Dev entrypoint: create the master key on first start, then run the command.
set -e
key="${REMAIL_MASTER_KEY_FILE:-/etc/remail/master.key}"
if [ "$1" = "serve" ] && [ ! -f "$key" ]; then
  remail-server key init
fi
exec remail-server "$@"
