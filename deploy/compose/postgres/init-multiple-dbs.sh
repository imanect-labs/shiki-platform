#!/bin/bash
# Postgres 初回起動時に Keycloak / OpenFGA / shiki 用の DB を作成する。
set -euo pipefail

create_db() {
  local db="$1"
  echo "creating database: $db"
  psql -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" <<-EOSQL
    SELECT 'CREATE DATABASE $db' WHERE NOT EXISTS (SELECT FROM pg_database WHERE datname = '$db')\gexec
EOSQL
}

for db in keycloak openfga shiki langfuse; do
  create_db "$db"
done
