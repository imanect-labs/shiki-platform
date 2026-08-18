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

# pg_stat_statements のビューを shiki DB に作る。shared_preload_libraries での読み込みは
# compose の postgres コマンドで済ませてあるが、統計を **読む** ための extension は DB ごとに要る。
# superuser が必要なため初期化時にここで作る（アプリの migration でやると、アプリユーザが
# superuser でないマネージド Postgres / オンプレ持込 Postgres で起動が落ちる）。
#
# 既存ボリュームでは本スクリプトは再実行されない。その場合は手で 1 回:
#   docker compose exec postgres psql -U postgres -d shiki -c \
#     'CREATE EXTENSION IF NOT EXISTS pg_stat_statements'
psql -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" --dbname shiki \
  -c 'CREATE EXTENSION IF NOT EXISTS pg_stat_statements'
