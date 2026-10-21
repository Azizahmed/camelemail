#!/usr/bin/env bash
# Start a throwaway Postgres for remail-server's integration tests.
#   Container remail-test-pg (postgres:16-alpine) on 127.0.0.1:55433, data in tmpfs.
# Each test creates and drops its own database. Then run:
#   REMAIL_TEST_DATABASE_URL=postgres://remail:remail@127.0.0.1:55433/postgres cargo test -p remail-server
# Stop it with: docker stop remail-test-pg
set -euo pipefail
name=remail-test-pg
port=55433
url="postgres://remail:remail@127.0.0.1:${port}/postgres"
if ! docker ps --format '{{.Names}}' | grep -qx "$name"; then
  docker run -d --rm --name "$name" \
    -e POSTGRES_USER=remail -e POSTGRES_PASSWORD=remail -e POSTGRES_DB=postgres \
    -p "127.0.0.1:${port}:5432" --tmpfs /var/lib/postgresql/data \
    postgres:16-alpine -c max_connections=400 -c fsync=off -c synchronous_commit=off >/dev/null
  for _ in $(seq 1 60); do
    if docker exec "$name" pg_isready -U remail -q 2>/dev/null; then break; fi
    sleep 0.5
  done
  # pg_isready can pass before the init scripts' restart; wait for a real query.
  for _ in $(seq 1 60); do
    if docker exec "$name" psql -U remail -d postgres -c 'select 1' >/dev/null 2>&1; then break; fi
    sleep 0.5
  done
  echo "Started $name on 127.0.0.1:${port}." >&2
else
  echo "$name is already running." >&2
fi
echo "export REMAIL_TEST_DATABASE_URL=${url}"
