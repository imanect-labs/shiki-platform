#!/usr/bin/env bash
# 再配布しないテストフィクスチャを取得する（トラックJTD）。
#
# 厚生労働省の 2 本（f1.jtd / betu.jtd）は CC BY で crates/jtd/tests/fixtures/ に同梱済み。
# ここで取るのは、再配布条件が未確認で同梱していないものだけ。
#
#   scripts/fetch-jtd-fixtures.sh [出力先ディレクトリ]
#
# 既定の出力先は crates/jtd/tests/fixtures/external/（.gitignore 済み）。
# **取得に失敗したら失敗させる。** 「取れなかったのでスキップ」にすると、
# 意図したテストが走らないまま緑になる（CLAUDE.md）。
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
DST="${1:-$ROOT/crates/jtd/tests/fixtures/external}"
mkdir -p "$DST"

# name|url|出所
FIXTURES=(
  "tpwin_jp.jtd|https://www.jci-net.or.jp/rally/2007/denshi/write/tpwin_jp.jtd|日本コンクリート工学会 和文原稿作成テンプレート（一太郎 Ver.13）"
  "tpwin_jp.doc|https://www.jci-net.or.jp/rally/2007/denshi/write/tpwin_jp.doc|同 Word 版（構造比較の二次オラクル）"
)

failed=0
for entry in "${FIXTURES[@]}"; do
  IFS='|' read -r name url origin <<<"$entry"
  echo "→ $name（$origin）"
  if ! curl -fsSL --retry 3 --retry-delay 2 -A "Mozilla/5.0" -o "$DST/$name.tmp" "$url"; then
    echo "   ❌ 取得できませんでした: $url" >&2
    rm -f "$DST/$name.tmp"
    failed=1
    continue
  fi
  # 取得できても中身が HTML のエラーページということがある。素性を確かめる。
  if [ ! -s "$DST/$name.tmp" ]; then
    echo "   ❌ 空のファイルが返りました: $url" >&2
    rm -f "$DST/$name.tmp"
    failed=1
    continue
  fi
  if ! head -c 8 "$DST/$name.tmp" | grep -q $'\xd0\xcf\x11\xe0'; then
    echo "   ❌ CFB（複合文書）ではありません。配布ページの構成が変わった可能性があります: $url" >&2
    rm -f "$DST/$name.tmp"
    failed=1
    continue
  fi
  mv "$DST/$name.tmp" "$DST/$name"
  echo "   ✅ $(wc -c <"$DST/$name") bytes"
done

if [ "$failed" -ne 0 ]; then
  cat >&2 <<'EOF'

❌ 取得できなかったフィクスチャがあります。
   ネットワークが無い環境ならそれで構いませんが、**このスクリプトを前提にした検証は
   走っていない**という扱いにしてください（スキップして緑にしない）。
EOF
  exit 1
fi

echo "✅ 取得完了: $DST"
