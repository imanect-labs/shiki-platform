#!/usr/bin/env bash
# Keycloak User Profile のスモークテスト（#809）。
#
# tenant_id / roles claim の取得元であるユーザー属性 `tenant` / `roles` を、
# **ユーザー本人が書き換えられない**（＝次のログインで他テナントに入れない）ことを実機で確かめる。
#   1. realm の User Profile 設定: tenant / roles が view・edit とも admin のみ、unmanaged 属性は無効
#   2. 一般ユーザーの Account REST API（/realms/<realm>/account）:
#      - GET に tenant / roles が出ない
#      - POST で tenant / roles を書き込もうとしても反映されない（再ログイン後の claim も不変）
#      - POST で未宣言（unmanaged）属性を足せない
#      - 氏名だけの通常の更新で tenant / roles が消えない
#   3. 管理者（admin REST・crates/api/src/keycloak_admin.rs と同じ経路）:
#      - attributes 付きのユーザー作成 → tenant 属性検索（q=tenant:..）で見つかる
#      - GET → attributes.tenant を差し替えて PUT（update_user_tenant と同じ）で反映される
#
# 前提: compose の Keycloak（dev realm fixture）。一般ユーザーのトークンは shiki-web client の
# Authorization Code + PKCE を curl で駆動して取る（shiki-server は不要）。
set -euo pipefail

KC="${KC:-http://localhost:8081}"
REALM="${REALM:-shiki}"
KC_ADMIN="${KEYCLOAK_ADMIN:-admin}"
KC_ADMIN_PASSWORD="${KEYCLOAK_ADMIN_PASSWORD:-admin}"
SMOKE_USER="${SMOKE_USER:-alice}"
SMOKE_PASSWORD="${SMOKE_PASSWORD:-password}"
CLIENT_ID="${CLIENT_ID:-shiki-web}"
CLIENT_SECRET="${CLIENT_SECRET:-shiki-web-dev-secret}"
# shiki-web の redirectUris（http://localhost:3000/*）に一致させる。code はこちらで受け取るだけ。
REDIRECT_URI="${REDIRECT_URI:-http://localhost:3000/smoke-user-profile-cb}"

ADMIN_API="$KC/admin/realms/$REALM"
TMP="$(mktemp -d)"
PROBE_ID=""
USER_ID=""
ORIG_ATTRS=""

fail() { echo "FAIL: $*" >&2; exit 1; }

admin_token() {
  curl -fsS -d grant_type=password -d client_id=admin-cli \
    --data-urlencode "username=$KC_ADMIN" --data-urlencode "password=$KC_ADMIN_PASSWORD" \
    "$KC/realms/master/protocol/openid-connect/token" \
    | python3 -c "import sys,json;print(json.load(sys.stdin)['access_token'])"
}

# 一時ユーザーの削除と、万一書き換わった一般ユーザーの tenant / roles の復元（共有 dev 環境を汚さない）。
cleanup() {
  local t
  t="$(admin_token 2>/dev/null || true)"
  if [ -n "$t" ] && [ -n "$PROBE_ID" ]; then
    curl -sS -o /dev/null -X DELETE -H "Authorization: Bearer $t" "$ADMIN_API/users/$PROBE_ID" || true
  fi
  if [ -n "$t" ] && [ -n "$USER_ID" ] && [ -n "$ORIG_ATTRS" ]; then
    restore_user_attrs "$t" || echo "WARN: $SMOKE_USER の属性復元に失敗しました" >&2
  fi
  rm -rf "$TMP"
}
trap cleanup EXIT

# 現在の representation を取り、tenant / roles を元の値へ戻し、未宣言属性を落として PUT する。
restore_user_attrs() {
  local t="$1"
  curl -fsS -H "Authorization: Bearer $t" "$ADMIN_API/users/$USER_ID" > "$TMP/cur.json"
  python3 - "$TMP/cur.json" "$ORIG_ATTRS" > "$TMP/restore.json" <<'PY'
import json, sys
u = json.load(open(sys.argv[1]))
orig = json.loads(sys.argv[2])
if u.get("attributes") == orig:
    sys.exit(3)
u["attributes"] = orig
print(json.dumps(u))
PY
  local rc=$?
  [ "$rc" = 3 ] && return 0
  [ "$rc" = 0 ] || return 1
  curl -fsS -o /dev/null -X PUT -H "Authorization: Bearer $t" -H 'Content-Type: application/json' \
    --data @"$TMP/restore.json" "$ADMIN_API/users/$USER_ID"
  echo "（$SMOKE_USER の tenant / roles を元へ戻しました）"
}

# Authorization Code + PKCE を curl で駆動して一般ユーザーのアクセストークンを取る。
user_token() {
  local jar="$TMP/kc-cookies" page="$TMP/login.html" verifier challenge action loc code
  rm -f "$jar"
  verifier="$(python3 -c 'import secrets;print(secrets.token_urlsafe(48))')"
  challenge="$(python3 -c 'import sys,hashlib,base64;print(base64.urlsafe_b64encode(hashlib.sha256(sys.argv[1].encode()).digest()).rstrip(b"=").decode())' "$verifier")"
  curl -fsS -c "$jar" -b "$jar" -G "$KC/realms/$REALM/protocol/openid-connect/auth" \
    --data-urlencode "client_id=$CLIENT_ID" --data-urlencode response_type=code \
    --data-urlencode scope=openid --data-urlencode "redirect_uri=$REDIRECT_URI" \
    --data-urlencode "code_challenge=$challenge" --data-urlencode code_challenge_method=S256 \
    --data-urlencode state=smoke -o "$page"
  action="$(grep -oE 'action="[^"]+"' "$page" | head -1 | sed -E 's/action="([^"]+)"/\1/' | sed 's/&amp;/\&/g')"
  [ -n "$action" ] || fail "ログインフォームの action が取れません"
  loc="$(curl -sS -b "$jar" -c "$jar" -o /dev/null -D - \
    --data-urlencode "username=$SMOKE_USER" --data-urlencode "password=$SMOKE_PASSWORD" "$action" \
    | grep -i '^location:' | tr -d '\r' | sed -E 's/^location:[[:space:]]*//I')"
  code="$(python3 -c 'import sys,urllib.parse as u;print(u.parse_qs(u.urlparse(sys.argv[1]).query).get("code",[""])[0])' "$loc")"
  [ -n "$code" ] || fail "認可コードが取れません（資格情報 POST 失敗: location=$loc）"
  curl -fsS -d grant_type=authorization_code -d "client_id=$CLIENT_ID" -d "client_secret=$CLIENT_SECRET" \
    --data-urlencode "code=$code" --data-urlencode "redirect_uri=$REDIRECT_URI" -d "code_verifier=$verifier" \
    "$KC/realms/$REALM/protocol/openid-connect/token" \
    | python3 -c "import sys,json;print(json.load(sys.stdin)['access_token'])"
}

# JWT の claim を 1 つ JSON で取り出す（署名検証はしない・表示/比較用）。
claim() {
  python3 -c '
import sys, json, base64
p = sys.argv[1].split(".")[1]; p += "=" * (-len(p) % 4)
print(json.dumps(json.loads(base64.urlsafe_b64decode(p)).get(sys.argv[2])))' "$1" "$2"
}

AT_ADMIN="$(admin_token)"

echo "--- 1. User Profile 設定: tenant / roles は admin のみ・unmanaged 属性は無効 ---"
curl -fsS -H "Authorization: Bearer $AT_ADMIN" "$ADMIN_API/users/profile" > "$TMP/up.json"
python3 - "$TMP/up.json" <<'PY'
import json, sys
up = json.load(open(sys.argv[1]))
policy = up.get("unmanagedAttributePolicy")
assert policy is None, f"unmanagedAttributePolicy={policy!r}（無効＝未設定であること）"
attrs = {a["name"]: a for a in up.get("attributes", [])}
for name, multi in (("tenant", False), ("roles", True)):
    a = attrs.get(name)
    assert a is not None, f"属性 {name} が User Profile に宣言されていません"
    perms = a.get("permissions") or {}
    for k in ("view", "edit"):
        assert sorted(perms.get(k) or []) == ["admin"], f"{name}.permissions.{k}={perms.get(k)!r}（['admin'] のみであること）"
    assert bool(a.get("multivalued")) == multi, f"{name}.multivalued={a.get('multivalued')!r}"
print("User Profile OK: tenant/roles=admin のみ, unmanagedAttributePolicy=無効")
PY

USER_ID="$(curl -fsS -H "Authorization: Bearer $AT_ADMIN" -G "$ADMIN_API/users" \
  --data-urlencode "username=$SMOKE_USER" --data-urlencode exact=true \
  | python3 -c "import sys,json;print(json.load(sys.stdin)[0]['id'])")"
ORIG_ATTRS="$(curl -fsS -H "Authorization: Bearer $AT_ADMIN" "$ADMIN_API/users/$USER_ID" \
  | python3 -c "import sys,json;print(json.dumps(json.load(sys.stdin).get('attributes') or {}, sort_keys=True))")"
ORIG_TENANT="$(python3 -c "import sys,json;print(json.dumps(json.loads(sys.argv[1]).get('tenant',[None])[0]))" "$ORIG_ATTRS")"
[ "$ORIG_TENANT" != "null" ] || fail "$SMOKE_USER に tenant 属性がありません（admin GET で見えない＝User Profile 未宣言の疑い）"
echo "$SMOKE_USER: id=$USER_ID attributes=$ORIG_ATTRS"

echo "--- 2a. 一般ユーザーのトークン（tenant claim の確認） ---"
AT_USER="$(user_token)"
[ "$(claim "$AT_USER" tenant)" = "$ORIG_TENANT" ] || fail "tenant claim=$(claim "$AT_USER" tenant)（期待 $ORIG_TENANT）"
echo "tenant claim=$(claim "$AT_USER" tenant) aud=$(claim "$AT_USER" aud)"

echo "--- 2b. Account API GET に tenant / roles が出ない ---"
curl -fsS -H "Authorization: Bearer $AT_USER" -H 'Accept: application/json' \
  "$KC/realms/$REALM/account" > "$TMP/account.json"
python3 - "$TMP/account.json" <<'PY'
import json, sys
acc = json.load(open(sys.argv[1]))
leaked = [k for k in ("tenant", "roles") if k in (acc.get("attributes") or {})]
meta = [a["name"] for a in (acc.get("userProfileMetadata") or {}).get("attributes", [])]
leaked += [f"metadata:{k}" for k in ("tenant", "roles") if k in meta]
assert not leaked, f"Account API に管理者専用属性が見えています: {leaked}"
print("Account GET OK: tenant/roles 非表示, 編集可能属性 =", meta)
PY

# 本人の representation（username/email/氏名）を下敷きに attributes だけ足して POST する。
account_post() {
  python3 - "$TMP/account.json" "$1" > "$TMP/post.json" <<'PY'
import json, sys
acc = json.load(open(sys.argv[1]))
body = {k: acc[k] for k in ("username", "email", "firstName", "lastName") if k in acc}
extra = json.loads(sys.argv[2])
if extra:
    body["attributes"] = extra
print(json.dumps(body))
PY
  curl -sS -o "$TMP/post-resp.txt" -w '%{http_code}' -X POST \
    -H "Authorization: Bearer $AT_USER" -H 'Content-Type: application/json' \
    --data @"$TMP/post.json" "$KC/realms/$REALM/account"
}

current_attrs() {
  curl -fsS -H "Authorization: Bearer $AT_ADMIN" "$ADMIN_API/users/$USER_ID" \
    | python3 -c "import sys,json;print(json.dumps(json.load(sys.stdin).get('attributes') or {}, sort_keys=True))"
}

echo "--- 2c. Account API POST で tenant / roles を書き込めない ---"
code="$(account_post '{"tenant":["smoke-other-tenant"],"roles":["smoke-escalated"],"smoke-unmanaged":["x"]}')"
echo "POST /account (tenant 改ざん) → HTTP $code $(head -c 300 "$TMP/post-resp.txt")"
case "$code" in 2??|4??) ;; *) fail "想定外のステータス $code" ;; esac
AT_ADMIN="$(admin_token)"
after="$(current_attrs)"
[ "$after" = "$ORIG_ATTRS" ] || fail "一般ユーザーが属性を書き換えられました: $ORIG_ATTRS → $after"
echo "admin GET attributes 不変 OK: $after"

AT_USER="$(user_token)"
[ "$(claim "$AT_USER" tenant)" = "$ORIG_TENANT" ] || fail "再ログイン後の tenant claim=$(claim "$AT_USER" tenant)（期待 $ORIG_TENANT）"
echo "再ログイン後の tenant claim 不変 OK: $(claim "$AT_USER" tenant)"

# tenant / roles の read-only 拒否（400）に紛れないよう、未宣言属性だけを単独で送る。
echo "--- 2d. Account API POST で未宣言属性を足せない（unmanaged 属性 無効） ---"
code="$(account_post '{"smoke-unmanaged":["x"]}')"
echo "POST /account (未宣言属性) → HTTP $code $(head -c 300 "$TMP/post-resp.txt")"
case "$code" in 2??|4??) ;; *) fail "想定外のステータス $code" ;; esac
after="$(current_attrs)"
[ "$after" = "$ORIG_ATTRS" ] || fail "一般ユーザーが未宣言属性を足せました: $ORIG_ATTRS → $after"
leaked="$(curl -fsS -H "Authorization: Bearer $AT_ADMIN" -G "$ADMIN_API/users" \
  --data-urlencode "q=smoke-unmanaged:x" | python3 -c "import sys,json;print(len(json.load(sys.stdin)))")"
[ "$leaked" = "0" ] || fail "未宣言属性 smoke-unmanaged が保存されています（$leaked 件）"
echo "未宣言属性 非保存 OK"

echo "--- 2e. 氏名だけの通常の更新で tenant / roles が消えない ---"
code="$(account_post '{}')"
[ "$code" = "204" ] || fail "通常のプロフィール更新が HTTP $code: $(head -c 300 "$TMP/post-resp.txt")"
after="$(current_attrs)"
[ "$after" = "$ORIG_ATTRS" ] || fail "通常のプロフィール更新で属性が変わりました: $ORIG_ATTRS → $after"
echo "通常更新後も attributes 不変 OK"

echo "--- 3. 管理者は tenant / roles を設定・検索・更新できる（keycloak_admin.rs と同じ経路） ---"
PROBE_TENANT="smoke-up-$(python3 -c 'import secrets;print(secrets.token_hex(4))')"
PROBE_USER="$PROBE_TENANT-admin"
code="$(curl -sS -o "$TMP/create.txt" -w '%{http_code}' -X POST -H "Authorization: Bearer $AT_ADMIN" \
  -H 'Content-Type: application/json' "$ADMIN_API/users" -d "{
    \"username\": \"$PROBE_USER\", \"email\": \"$PROBE_USER@example.com\", \"enabled\": true,
    \"emailVerified\": true, \"attributes\": {\"tenant\": [\"$PROBE_TENANT\"], \"roles\": [\"engineering\", \"sales\"]}
  }")"
[ "$code" = "201" ] || fail "admin のユーザー作成が HTTP $code: $(cat "$TMP/create.txt")"
curl -fsS -H "Authorization: Bearer $AT_ADMIN" -G "$ADMIN_API/users" \
  --data-urlencode "q=tenant:$PROBE_TENANT" > "$TMP/search.json"
PROBE_ID="$(python3 - "$TMP/search.json" "$PROBE_USER" "$PROBE_TENANT" <<'PY'
import json, sys
users = json.load(open(sys.argv[1]))
assert [u["username"] for u in users] == [sys.argv[2]], f"q=tenant 検索結果が不正: {users}"
attrs = users[0].get("attributes") or {}
assert attrs.get("tenant") == [sys.argv[3]], f"tenant 属性が保存されていません: {attrs}"
assert sorted(attrs.get("roles") or []) == ["engineering", "sales"], f"roles 属性が保存されていません: {attrs}"
print(users[0]["id"])
PY
)"
echo "admin 作成＋tenant 検索 OK: $PROBE_USER ($PROBE_ID)"

curl -fsS -H "Authorization: Bearer $AT_ADMIN" "$ADMIN_API/users/$PROBE_ID" \
  | python3 -c "import sys,json;u=json.load(sys.stdin);u['attributes']['tenant']=['$PROBE_TENANT-moved'];print(json.dumps(u))" \
  > "$TMP/put.json"
code="$(curl -sS -o "$TMP/put-resp.txt" -w '%{http_code}' -X PUT -H "Authorization: Bearer $AT_ADMIN" \
  -H 'Content-Type: application/json' --data @"$TMP/put.json" "$ADMIN_API/users/$PROBE_ID")"
[ "$code" = "204" ] || fail "admin の tenant 更新が HTTP $code: $(cat "$TMP/put-resp.txt")"
moved="$(curl -fsS -H "Authorization: Bearer $AT_ADMIN" "$ADMIN_API/users/$PROBE_ID" \
  | python3 -c "import sys,json;print(json.load(sys.stdin)['attributes'].get('tenant'))")"
[ "$moved" = "['$PROBE_TENANT-moved']" ] || fail "admin の tenant 更新が反映されません: $moved"
echo "admin tenant 更新 OK: $moved"

echo "Keycloak User Profile smoke OK"
