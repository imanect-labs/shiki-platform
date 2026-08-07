#!/usr/bin/env bash
# migration のバージョン番号が重複していないかを検査する CI ゲート（#413 の続き）。
#
# 背景: `0057_node_system.sql`（#396）と `0057_share_link_grant_revoke.sql`（#393）が
# **別 PR から同じ番号で**入り、`sqlx::migrate!` が新規 DB で
#   duplicate key value violates unique constraint "_sqlx_migrations_pkey" (version=57)
# を出して停止した。`_sqlx_migrations` の主キーは version なので、同番の 2 本目は必ず失敗する。
# しかも**先に適用された 1 本目だけが入り 2 本目の DDL は入らない**（今回は `node.system` 列が
# 新規 DB に存在しない状態になった）。既存 DB は増分適用済みで露見しないため、
# 新規 cell プロビジョニング（NFR-11）と新規開発環境でだけ壊れる＝気付きにくい。
#
# 番号の採番はマージ順に依存するので、並行 PR では衝突が構造的に起こり得る。人間のレビューに
# 頼らずここで機械的に止める。
#
# 検査内容:
#   1. バージョン番号の重複が無いこと（本題）
#   2. `<version>_<description>.sql` の形式であること（sqlx がパースできない名前を弾く）
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

MIGRATIONS_DIR="${MIGRATIONS_DIR:-migrations}"

if [ ! -d "$MIGRATIONS_DIR" ]; then
  echo "❌ migration ディレクトリが見つかりません: $MIGRATIONS_DIR" >&2
  exit 1
fi

status=0

# --- 1. 命名規則（sqlx は先頭の数値を version として読む） ---
malformed=()
while IFS= read -r name; do
  [ -n "$name" ] || continue
  if ! [[ "$name" =~ ^[0-9]+_[^/]+\.sql$ ]]; then
    malformed+=("$name")
  fi
done < <(git ls-files "$MIGRATIONS_DIR" | sed 's|.*/||' | grep -E '\.sql$' || true)

if [ "${#malformed[@]}" -gt 0 ]; then
  echo "❌ migration のファイル名が <version>_<description>.sql 形式ではありません:" >&2
  printf '  %s\n' "${malformed[@]}" >&2
  status=1
fi

# --- 2. バージョン重複（本題） ---
# 先頭の数値部分を version として取り出す。0057 と 57 は sqlx から見て同じ 57 なので、
# 先行ゼロを落として比較する。
versions_with_files=$(
  git ls-files "$MIGRATIONS_DIR" \
    | sed 's|.*/||' \
    | grep -E '^[0-9]+_.*\.sql$' \
    | while IFS= read -r name; do
        raw="${name%%_*}"
        # 10 進として解釈（先行ゼロ対策。sed で先頭ゼロを除去し、空なら 0）。
        num=$(printf '%s' "$raw" | sed 's/^0*//')
        [ -n "$num" ] || num=0
        printf '%s\t%s\n' "$num" "$name"
      done
)

dupe_versions=$(printf '%s\n' "$versions_with_files" | cut -f1 | sort -n | uniq -d || true)

if [ -n "$dupe_versions" ]; then
  echo "❌ migration のバージョン番号が重複しています（sqlx は同番の 2 本目を適用できません）:" >&2
  while IFS= read -r v; do
    [ -n "$v" ] || continue
    echo "  version $v:" >&2
    printf '%s\n' "$versions_with_files" | awk -F'\t' -v ver="$v" '$1 == ver { printf "    %s\n", $2 }' >&2
  done <<< "$dupe_versions"
  echo >&2
  echo "→ 後からマージされた側を未使用の番号へリネームしてください。" >&2
  echo "  ⚠️ 既に適用済みの migration をリネーム/改変すると、その DB は checksum 不一致" >&2
  echo "     （sqlx の VersionMismatch）で起動できなくなります。まだどの DB にも適用されて" >&2
  echo "     いない側を動かすこと。判断が付かない場合は human に相談してください。" >&2
  status=1
fi

if [ "$status" -eq 0 ]; then
  total=$(printf '%s\n' "$versions_with_files" | grep -c . || true)
  echo "✅ migration のバージョンは一意です（${total} ファイル）。"
fi

exit "$status"
