#!/usr/bin/env bash
# Run Taro Enterprise locally for development and the web apps' browser checks.
#
#   scripts/dev-server.sh <name> [portal-port]       (the Provider Console is on portal-port + 1)
#
# Needs a Postgres server (PG_URL, default postgres://postgres@127.0.0.1:5432) and a built
# target/debug/taro-enterprise. Makes the database taro_dev_<name> if missing, adds the example
# plans, makes ops@example.test the first platform administrator, and serves:
#   Customer Portal   http://127.0.0.1:<port>      sign in with any address; codes are in
#   Provider Console  http://127.0.0.1:<port+1>    GET /api/v1/dev/outbox (development only)
# Payments are simulated (no Stripe keys) and the Macs come from the mock provider.
set -euo pipefail
name=${1:?usage: dev-server.sh <name> [portal-port]}
port=${2:-8080}
pg=${PG_URL:-postgres://postgres@127.0.0.1:5432}
here=$(cd "$(dirname "$0")/.." && pwd)
bin=${TARO_BIN:-$here/target/debug/taro-enterprise}
db="taro_dev_${name//-/_}"
psql "$pg/postgres" -tAc "SELECT 1 FROM pg_database WHERE datname = '$db'" | grep -q 1 \
  || psql "$pg/postgres" -qc "CREATE DATABASE $db"
export TARO_ENV=development
export TARO_DATABASE_URL="$pg/$db"
export TARO_LISTEN="127.0.0.1:$port" TARO_OPS_LISTEN="127.0.0.1:$((port + 1))"
export TARO_PUBLIC_URL="http://127.0.0.1:$port" TARO_OPS_URL="http://127.0.0.1:$((port + 1))"
export TARO_BOOTSTRAP_ADMIN_EMAIL=${TARO_BOOTSTRAP_ADMIN_EMAIL:-ops@example.test}
export TARO_MOCK_MACS=${TARO_MOCK_MACS:-6} TARO_MOCK_DELAY_SECS=${TARO_MOCK_DELAY_SECS:-2}
export TARO_WEB_DIR=${TARO_WEB_DIR:-$here}
export RUST_LOG=${RUST_LOG:-warn}
"$bin" seed-dev >/dev/null
exec "$bin" serve
