#!/bin/sh
# shiki-server の実行時ロール（ログイン用）を作る（#1030）。compose の postgres-roles サービスが
# 毎回の起動時に流す（冪等）。初回だけ走る init スクリプトに置くと、既存ボリュームに届かない。
#
#   shiki_runtime … DML だけを持つグループロール（NOLOGIN）。権限は migration 0110 が付ける。
#   shiki_app     … shiki-server が接続するログインロール。shiki_runtime のメンバ。
#
# migration は従来どおり所有者（POSTGRES_USER）の接続で流す（SHIKI__DATABASE__MIGRATE_URL）。
# shiki_runtime はここでも migration でも作る。どちらが先に走っても揃う。
set -eu

: "${SHIKI_APP_DB_PASSWORD:?SHIKI_APP_DB_PASSWORD が未設定です}"

# postgres の healthcheck（ソケットの pg_isready）は初期化中の一時サーバでも通るので、TCP で繋がり
# shiki DB ができるまで待つ（init-multiple-dbs.sh が作る）。
i=0
until psql --dbname shiki -tAc 'SELECT 1' >/dev/null 2>&1; do
  i=$((i + 1))
  [ "$i" -ge 60 ] && { echo "postgres（shiki DB）に接続できません" >&2; exit 1; }
  sleep 1
done

psql -v ON_ERROR_STOP=1 -v app_password="$SHIKI_APP_DB_PASSWORD" --dbname shiki <<'EOSQL'
SELECT 'CREATE ROLE shiki_runtime NOLOGIN NOSUPERUSER NOBYPASSRLS NOCREATEDB NOCREATEROLE'
 WHERE NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'shiki_runtime')\gexec
SELECT 'CREATE ROLE shiki_app LOGIN NOSUPERUSER NOBYPASSRLS NOCREATEDB NOCREATEROLE INHERIT'
 WHERE NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'shiki_app')\gexec
-- パスワードは毎回 .env の値に合わせる（.env を変えたら再起動で反映される）。
ALTER ROLE shiki_app WITH LOGIN NOSUPERUSER NOBYPASSRLS PASSWORD :'app_password';
GRANT shiki_runtime TO shiki_app;
GRANT CONNECT ON DATABASE shiki TO shiki_app;
EOSQL
echo "shiki_app（shiki_runtime のメンバ）を用意しました"
