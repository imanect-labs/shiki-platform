#!/usr/bin/env bash
# BFF（オパークセッション Cookie）方式のスモークテスト。
# curl のクッキージャーで OIDC Authorization Code フローを駆動し、/me まで通す。
#   1. GET /auth/login        → 302 authorize（相関 Cookie 発行）
#   2. GET authorize          → Keycloak ログインページ（KC Cookie 取得・form action 抽出）
#   3. POST 資格情報           → 302 redirect_uri（?code=...&state=...）
#   4. GET /auth/callback     → サーバ側 token 交換・セッション Cookie 発行
#   5. GET /me（Cookie）       → 200
#   6. GET /me（Cookie 無し）  → 401
set -euo pipefail

API="${API:-http://localhost:8080}"
CJ="$(mktemp)"   # アプリ（shiki-server）側 Cookie
KC="$(mktemp)"   # Keycloak 側 Cookie
LOGIN_HTML="$(mktemp)"
ME_JSON="$(mktemp)"
cleanup() { rm -f "$CJ" "$KC" "$LOGIN_HTML" "$ME_JSON" 2>/dev/null || true; }
trap cleanup EXIT

location_of() { grep -i '^location:' | sed -E 's/^location:[[:space:]]*//I' | tr -d '\r'; }

echo "--- /healthz ---"
curl -fsS "$API/healthz" >/dev/null

echo "--- 1. GET /auth/login → authorize URL ---"
AUTHZ="$(curl -sS -c "$CJ" -o /dev/null -D - "$API/auth/login" | location_of)"
test -n "$AUTHZ" || { echo "authorize URL が取れません"; exit 1; }

echo "--- 2. Keycloak ログインページ取得 ---"
curl -sS -c "$KC" -b "$KC" "$AUTHZ" -o "$LOGIN_HTML"
ACTION="$(grep -oE 'action="[^"]+"' "$LOGIN_HTML" | head -1 | sed -E 's/action="([^"]+)"/\1/' | sed 's/&amp;/\&/g')"
test -n "$ACTION" || { echo "ログインフォームの action が取れません"; exit 1; }

echo "--- 3. 資格情報 POST → callback URL ---"
CALLBACK="$(curl -sS -b "$KC" -c "$KC" -o /dev/null -D - \
  --data-urlencode "username=alice" --data-urlencode "password=password" "$ACTION" | location_of)"
test -n "$CALLBACK" || { echo "callback URL が取れません（資格情報 POST 失敗）"; exit 1; }

echo "--- 4. GET /auth/callback → セッション Cookie 発行 ---"
curl -sS -b "$CJ" -c "$CJ" -o /dev/null "$CALLBACK"

echo "--- 5. GET /me（セッション Cookie） ---"
code="$(curl -sS -b "$CJ" -o "$ME_JSON" -w '%{http_code}' "$API/me")"
echo "status=$code body=$(cat "$ME_JSON")"
test "$code" = "200" || { echo "/me がセッションで 200 を返しません"; exit 1; }
grep -q '"id"' "$ME_JSON" || { echo "/me のボディが不正"; exit 1; }

echo "--- 6. GET /me（Cookie 無し → 401） ---"
code401="$(curl -sS -o /dev/null -w '%{http_code}' "$API/me")"
test "$code401" = "401" || { echo "Cookie 無し /me が 401 を返しません"; exit 1; }

echo "--- セッション Cookie が不透明（JWT でない＝トークン非露出） ---"
SESS="$(grep -i 'shiki_session' "$CJ" | awk '{print $NF}')"
test -n "$SESS" || { echo "セッション Cookie がありません"; exit 1; }
case "$SESS" in
  *.*) echo "セッション Cookie が JWT 形状（トークン露出の疑い）"; exit 1 ;;
esac

echo "BFF smoke OK"
