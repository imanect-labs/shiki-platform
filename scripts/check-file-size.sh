#!/usr/bin/env bash
# 1 ファイルが巨大化する（AI 生成でありがち）のを規約として止める CI ゲート。
#
# 対象: git 管理下の Rust ソース（*.rs）。ただし以下は除外する:
#   - tests/            … 結合テストは網羅性のため長くなりがちで、責務分割の指標が異なる
#   - */generated/*     … 生成物（codegen が正・手を入れない）
#   - target/           … ビルド成果物
#   - vendor/           … 所有フォーク（secure-exec）。上流由来コードは同一基準で縛らない（docs/sandbox/fork-policy.md）
#
# しきい値は MAX_LINES（既定 500 行）。超過ファイルがあれば一覧を出して 1 で終了する。
# 「関数が巨大」は clippy の too_many_lines / cognitive_complexity で別途担保する。
set -euo pipefail

MAX_LINES="${MAX_LINES:-500}"

cd "$(git rev-parse --show-toplevel)"

# 対象ファイルの収集（NUL 区切りで空白/日本語パスにも耐性を持たせる）。
mapfile -d '' -t files < <(
  git ls-files -z -- '*.rs' \
    | { grep -zvE '(^|/)tests/|/generated/|(^|/)target/|(^|/)vendor/' || true; }
)

violations=()
for f in "${files[@]}"; do
  [ -f "$f" ] || continue
  lines=$(wc -l <"$f")
  if [ "$lines" -gt "$MAX_LINES" ]; then
    violations+=("$lines	$f")
  fi
done

if [ "${#violations[@]}" -gt 0 ]; then
  echo "❌ 1 ファイルの行数上限 (${MAX_LINES} 行) を超過しています:" >&2
  printf '%s\n' "${violations[@]}" | sort -rn | awk -F'\t' '{printf "  %6d 行  %s\n", $1, $2}' >&2
  echo >&2
  echo "→ 責務ごとにモジュール分割してください（同一 struct の impl は別ファイルに分けられます）。" >&2
  echo "  一時的に許容する場合のみ MAX_LINES を上げるか、正当な理由を PR で説明すること。" >&2
  exit 1
fi

echo "✅ 全 Rust ソースが ${MAX_LINES} 行以内です（対象 ${#files[@]} ファイル）。"
