#!/bin/bash
# Postgres and ClickHouse for the SQL-block integration tests
# (TaisceKitTests PostgresServerTests / ClickHouseServerTests), in Apple's
# `container` CLI if present, else Docker. Prints the variables to export;
# `sql-servers.sh stop` removes both containers.
#
#   eval "$(apple/scripts/sql-servers.sh)" && (cd apple/TaisceKit && swift test --filter ServerTests)
set -euo pipefail

PW=taisce-test
PG=taisce-sql-pg
CH=taisce-sql-ch

if command -v container >/dev/null 2>&1 && container system status >/dev/null 2>&1; then
  RT=container
elif command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
  RT=docker
else
  echo "neither container nor docker is running" >&2
  exit 1
fi

if [ "${1:-}" = stop ]; then
  $RT stop $PG $CH >/dev/null 2>&1 || true
  $RT rm $PG $CH >/dev/null 2>&1 || true
  exit 0
fi

running() { $RT inspect "$1" >/dev/null 2>&1; }

if [ $RT = container ]; then
  running $PG || container run -d --name $PG -e POSTGRES_PASSWORD=$PW -e POSTGRES_DB=taisce docker.io/library/postgres:18 >&2
  running $CH || container run -d --name $CH -e CLICKHOUSE_USER=taisce -e CLICKHOUSE_PASSWORD=$PW -e CLICKHOUSE_DB=taisce docker.io/clickhouse/clickhouse-server:latest >&2
  ip() { container inspect "$1" | python3 -c 'import json,sys; print(json.load(sys.stdin)[0]["status"]["networks"][0]["ipv4Address"].split("/")[0])'; }
  PG_ADDR="$(ip $PG):5432"
  CH_URL="http://$(ip $CH):8123"
else
  running $PG || docker run -d --name $PG -p 127.0.0.1:55432:5432 -e POSTGRES_PASSWORD=$PW -e POSTGRES_DB=taisce postgres:18 >&2
  running $CH || docker run -d --name $CH -p 127.0.0.1:58123:8123 -e CLICKHOUSE_USER=taisce -e CLICKHOUSE_PASSWORD=$PW -e CLICKHOUSE_DB=taisce clickhouse/clickhouse-server:latest >&2
  PG_ADDR="127.0.0.1:55432"
  CH_URL="http://127.0.0.1:58123"
fi

# wait until both answer
for _ in $(seq 1 60); do
  if curl -sf "$CH_URL/ping" >/dev/null 2>&1 && nc -z "${PG_ADDR%:*}" "${PG_ADDR#*:}" >/dev/null 2>&1; then break; fi
  sleep 1
done

echo "export TAISCE_SQL_PG=$PG_ADDR TAISCE_SQL_PG_PASSWORD=$PW TAISCE_SQL_CH=$CH_URL TAISCE_SQL_CH_PASSWORD=$PW"
