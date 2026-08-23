#!/usr/bin/env bash
# OpenJTD フォークを再 vendor する（上流の特定 commit からサブセットを取り込む）。
# 所有フォークなので上流追従は任意（docs/jtd/fork-policy.md）。必要な修正を取り込むときに使う。
#
# 使い方: scripts/update-openjtd.sh <upstream-commit-sha>
#   1. 上流を一時 clone → 指定 commit を checkout
#   2. **一時 clone に対して patches/*.patch をドライラン**（ここで落ちたら何も壊さず終了）
#   3. vendor 対象サブセット（rjtd ワークスペース・openjtd-spec・docs・ライセンス）を同期
#   4. patches/*.patch を順に適用
#   5. 動作確認（shiki が依存する rjtd-core と、解読プローブの rjtd-cli）
#
# patches/ に記録されていない改変は、この手順で**無言で消える**。vendored ツリーへ直接手を
# 入れたら必ず patches/ にも切り出すこと（docs/jtd/fork-policy.md）。
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

# パッチが当たるかを**先に**確かめる。順序が逆だと、適用に失敗した瞬間に
# パッチの当たっていない（＝既知の穴が開いた）上流ソースが作業ツリーに残る。
# 上流がこちらの修正を取り込むと、その patch は必ずここで落ちる（それが期待動作）。
echo "→ patches ドライラン"
if compgen -G "$DST/patches/*.patch" >/dev/null; then
  for p in "$DST/patches"/*.patch; do
    if ! git -C "$TMP/src" apply --check "$p" 2>/dev/null; then
      cat >&2 <<EOF
❌ $(basename "$p") が $SHA に適用できません。

上流が同等の修正を取り込んだ可能性があります。その場合は
  1. 上流の該当箇所を読み、我々の修正が不要になったことを確かめる
  2. $DST/patches/$(basename "$p") を削除する
  3. $DST/UPSTREAM の「ローカルパッチ」節から該当項目を消す
を行ってから再実行してください。作業ツリーは変更していません。
EOF
      exit 1
    fi
    git -C "$TMP/src" apply "$p"
  done
fi

echo "→ rjtd ワークスペース同期（ビルド成果物は除く・patches 適用済み）"
rm -rf "$DST/rjtd"
cp -a "$TMP/src/rjtd" "$DST/"
rm -rf "$DST/rjtd/target"

echo "→ 仕様 RFC / 上流ノート / ライセンス同期"
rm -rf "$DST/openjtd-spec" "$DST/docs"
cp -a "$TMP/src/openjtd-spec" "$TMP/src/docs" "$DST/"
for f in LICENSE THIRD_PARTY.md README.md README.ja.md TODO.md TODO.ja.md; do
  cp -a "$TMP/src/$f" "$DST/$f"
done

echo "→ UPSTREAM の commit を更新"
sed -i "s|^commit:.*|commit:     $SHA|" "$DST/UPSTREAM"
sed -i "s|^vendored:.*|vendored:   $(date +%Y-%m-%d)|" "$DST/UPSTREAM"

echo "→ ビルド確認（shiki が依存する rjtd-core ＋ 解読プローブの rjtd-cli）"
( cd "$ROOT" && cargo build -p shiki-jtd )
( cd "$DST/rjtd" && cargo build -p rjtd-cli )

echo "→ 敵対的入力の回帰テスト"
( cd "$ROOT" && cargo test -p shiki-jtd )

echo "✅ 再 vendor 完了。crates/jtd 側のゴールデンテストも流して退行が無いか確認すること。"
