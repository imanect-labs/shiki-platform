#!/usr/bin/env bash
# ts-rs の書き出し（crates/*/bindings/*.ts）が、Rust の型の定義と合っているかを確かめる。
#
# `#[ts(export)]` を持つクレートごとに、書き出しのテスト（ts-rs が作る `export_bindings_*`）を、作業ツリーの外の
# 空のディレクトリ（TS_RS_EXPORT_DIR）へ書き出させ、commit してある bindings と突き合わせる。次のどれかで落とす。
#   - 中身が違う（型を変えたのに書き出しを commit し忘れた）
#   - 書き出しにあって commit に無い（型を足した）
#   - commit にあって書き出しに無い（型を消した・名前を変えた）
#
# import の拡張子（`from "./X"` と `from "./X.js"`）の差は見ない。ts-rs の `import-esm` は、ワークスペースの
# 依存（vendor/secure-exec の vm-config）が有効にするので、`--workspace` で回すかどうかで向きが変わるだけで、
# 型の中身は同じ（.claude/skills/pr/references/gates.md）。
#
# 作業ツリーの bindings には書かない（ほかの変更の生成物を書き換えない）。テストの build は、CI の rust ジョブの
# `cargo nextest run --workspace --all-features` と同じ形で回すので、そのジョブの後ろでは作り直さない。
#
# 使い方: bash scripts/check-ts-bindings.sh   （cargo は呼ぶ側の環境のものを使う。CARGO で差し替えられる）
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"
CARGO="${CARGO:-cargo}"

OUT="$ROOT/target/ts-bindings-check"
rm -rf "$OUT"
mkdir -p "$OUT"

# import の拡張子をそろえる（`from "./X.js"` → `from "./X"`）。
normalize() {
  sed -E 's#(from "\./[^"]+)\.js"#\1"#' "$1"
}

mapfile -t CRATE_DIRS < <(grep -rl --include='*.rs' '#\[ts(export' crates | awk -F/ '{print $2}' | sort -u)
if [ "${#CRATE_DIRS[@]}" -eq 0 ]; then
  echo "::error::#[ts(export)] を持つクレートが見つかりません（検査の前提が崩れている）"
  exit 1
fi

fail=0
for dir in "${CRATE_DIRS[@]}"; do
  manifest="crates/$dir/Cargo.toml"
  pkg="$(sed -nE 's/^name[[:space:]]*=[[:space:]]*"([^"]+)".*/\1/p' "$manifest" | head -n1)"
  if [ -z "$pkg" ]; then
    echo "::error::$manifest からパッケージ名を読めません"
    exit 1
  fi
  gen="$OUT/$dir"
  mkdir -p "$gen"
  echo "== $pkg（crates/$dir/bindings）"
  TS_RS_EXPORT_DIR="$gen" "$CARGO" nextest run --workspace --all-features --no-tests=fail \
    --status-level fail --final-status-level fail \
    -E "package($pkg) & test(/export_bindings_/)"

  committed="crates/$dir/bindings"
  # 書き出したもの → commit してあるもの
  while IFS= read -r -d '' f; do
    rel="${f#"$gen"/}"
    if ! git ls-files --error-unmatch "$committed/$rel" >/dev/null 2>&1; then
      echo "::error file=$committed/$rel::書き出しにあるのに commit されていません（型を足したら書き出しも commit する）"
      fail=1
    elif ! diff -u <(git show ":$committed/$rel" | normalize /dev/stdin) <(normalize "$f") \
        --label "commit: $committed/$rel" --label "Rust の定義から書き出したもの"; then
      echo "::error file=$committed/$rel::commit してある書き出しが Rust の定義と違います"
      fail=1
    fi
  done < <(find "$gen" -type f -name '*.ts' -print0)
  # commit してあるもの → 書き出したもの
  while IFS= read -r rel; do
    if [ ! -f "$gen/${rel#"$committed"/}" ]; then
      echo "::error file=$rel::Rust の定義から書き出されません（型を消した・名前を変えたら、書き出しも消す）"
      fail=1
    fi
  done < <(git ls-files -- "$committed/*.ts")
done

if [ "$fail" -ne 0 ]; then
  echo "直し方: TS_RS_EXPORT_DIR を付けずに cargo test -p <クレート> export_bindings_ を回し、型の中身が変わった"
  echo "書き出しだけを commit する（import の拡張子だけの差は戻してよい）。消した型の書き出しは git rm する。"
  exit 1
fi
echo "ts-rs の書き出しは Rust の定義と合っています（${#CRATE_DIRS[@]} クレート）。"
