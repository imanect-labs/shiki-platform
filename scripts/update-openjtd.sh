#!/usr/bin/env bash
# OpenJTD フォークを再 vendor する（上流の特定 commit からサブセットを取り込む）。
# 所有フォークなので上流追従は任意（docs/jtd/fork-policy.md）。必要な修正を取り込むときに使う。
#
# 使い方: scripts/update-openjtd.sh <upstream-commit-sha>
#   1. 上流を一時 clone → 指定 commit を checkout
#   2. vendor 対象サブセット（rjtd ワークスペース・openjtd-spec・docs・ライセンス）を同期
#   3. patches/*.patch を順に適用
#   4. UPSTREAM の commit を更新
#   5. 動作確認（shiki が依存する rjtd-core / rjtd-model のビルド）
set -euo pipefail

SHA="${1:-}"
if [ -z "$SHA" ]; then
  echo "usage: $0 <upstream-commit-sha>" >&2
  exit 1
fi

ROOT="$(git rev-parse --show-toplevel)"
DST="$ROOT/vendor/openjtd"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "→ 上流 clone（$SHA）"
git clone --quiet https://github.com/KimEJ/OpenJTD "$TMP/src"
git -C "$TMP/src" checkout --quiet "$SHA"

echo "→ rjtd ワークスペース同期（ビルド成果物は除外・上流のまま無改変）"
rm -rf "$DST/rjtd"
rsync -a --exclude 'target/' "$TMP/src/rjtd" "$DST/"

echo "→ 仕様 RFC / 上流ノート / ライセンス同期"
rm -rf "$DST/openjtd-spec" "$DST/docs"
cp -a "$TMP/src/openjtd-spec" "$TMP/src/docs" "$DST/"
for f in LICENSE THIRD_PARTY.md README.md README.ja.md TODO.md TODO.ja.md; do
  cp -a "$TMP/src/$f" "$DST/$f"
done

echo "→ patches 適用"
if compgen -G "$DST/patches/*.patch" >/dev/null; then
  for p in "$DST/patches"/*.patch; do
    echo "   apply $(basename "$p")"
    git -C "$ROOT" apply --directory="vendor/openjtd" "$p"
  done
fi

echo "→ UPSTREAM の commit を更新"
sed -i "s|^commit:.*|commit:     $SHA|" "$DST/UPSTREAM"
sed -i "s|^vendored:.*|vendored:   $(date +%Y-%m-%d)|" "$DST/UPSTREAM"

echo "→ ビルド確認（shiki が依存する 2 crate のみ）"
( cd "$ROOT" && cargo build -p shiki-jtd )

echo "✅ 再 vendor 完了。crates/jtd 側のゴールデンテストを流して退行が無いか確認すること。"
