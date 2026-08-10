#!/usr/bin/env bash
# local-gates.sh — CI と同じ品質ゲートを、差分に応じて選んでローカルで回す。
#
#   使い方:
#     local-gates.sh [--fast] [--base <ref>]
#
#   --fast : 重いもの（cargo deny / pnpm build / pytest / tabular runner）を省く。
#            早い段階の自己チェック用。push 前には必ずフルで回すこと。
#
#   判定は `git diff --name-only <base>...HEAD` ＋ 未コミットの差分。
#   各ゲートの合否を最後に一覧し、1 つでも落ちたら exit 1 で終わる。
#
#   注意: `cmd | tail` はパイプ終端の exit code を返すため合否が化ける。
#         このスクリプトは常にログへリダイレクトして `$?` を直接見る。
set -uo pipefail   # -e は付けない（落ちたゲートも記録して先へ進む）

FAST=0
BASE=""
while [ $# -gt 0 ]; do
  case "$1" in
    --fast) FAST=1 ;;
    --base) BASE="${2:-}"; shift ;;
    -h|--help) sed -n '2,18p' "$0"; exit 0 ;;
    *) printf 'unknown arg: %s\n' "$1" >&2; exit 2 ;;
  esac
  shift
done

ROOT=$(git rev-parse --show-toplevel 2>/dev/null) || { echo "git リポジトリ内で実行してください。" >&2; exit 2; }
cd "$ROOT"

# base は必ず remote-tracking ref を使う。ローカルの `main` は古いことが多く、
# `main...HEAD` が「このブランチで追加した」ファイルを何千件も誤検出する
# （実測: ローカル main 基準 2427 件 / origin/main 基準 1 件）。
if [ -z "$BASE" ]; then
  BASE=$(git symbolic-ref --short refs/remotes/origin/HEAD 2>/dev/null) || true
  BASE=${BASE:-origin/main}
elif git rev-parse --verify --quiet "origin/$BASE" >/dev/null; then
  BASE="origin/$BASE"
fi
git rev-parse --verify --quiet "$BASE" >/dev/null || { echo "base ref が見つかりません: $BASE（git fetch origin してください）" >&2; exit 2; }

LOGDIR="${TMPDIR:-/tmp}/shiki-gates-$(id -u)"
mkdir -p "$LOGDIR"

# コミット済み差分 ＋ 未コミット差分（新規ファイルは git add 済みのものだけが見える）。
CHANGED=$(
  { git diff --name-only "$BASE"...HEAD 2>/dev/null || true
    git diff --name-only HEAD 2>/dev/null || true
    git diff --cached --name-only 2>/dev/null || true
  } | sort -u
)

# ⚠️ パイプで渡さないこと。`printf ... | grep -Eq` は grep が最初のマッチで即終了するため
# printf が SIGPIPE で死に、`set -o pipefail` により rc=141 が返る。すると「差分に該当する」
# のに touches が偽となり、**ゲートが黙ってスキップされたまま「全ゲート通過」と報告される**
# （差分がパイプバッファ 64KB を超えると発生。古い base を指定した時などに実際に起きる）。
touches() { grep -Eq "$1" <<<"$CHANGED"; }

# 未追跡の .rs があると check-file-size.sh をすり抜けて CI で落ちる。
UNTRACKED_RS=$(git ls-files --others --exclude-standard -- '*.rs' 2>/dev/null || true)
if [ -n "$UNTRACKED_RS" ]; then
  echo "⚠️  git add されていない .rs があります（file-size ゲートをすり抜け、CI で落ちます）:"
  printf '%s\n' "$UNTRACKED_RS" | sed 's/^/   /'
  echo
fi

RESULTS=""
FAILED=0
SKIPPED=0

run_gate() {
  local name="$1"; shift
  local log="$LOGDIR/$(printf '%s' "$name" | tr ' /:' '___').log"
  printf '\n=== %s ===\n' "$name"
  if "$@" > "$log" 2>&1; then
    printf '   ✅ %s\n' "$name"
    RESULTS="${RESULTS}✅ ${name}\n"
  else
    printf '   ❌ %s  (log: %s)\n' "$name" "$log"
    grep -nE '^error|^error\[|could not compile|FAILED|failed:|✖|Error:' "$log" | head -15 | sed 's/^/      /'
    RESULTS="${RESULTS}❌ ${name}  → ${log}\n"
    FAILED=1
  fi
}

sh_c() { bash -c "$1"; }

echo "base: $BASE"
echo "変更ファイル: $(printf '%s\n' "$CHANGED" | grep -c . ) 件"
echo "ログ: $LOGDIR"

# ---------- 常に回す（CI の quality ジョブと同一） ----------
run_gate "file-size (1ファイル500行)" bash scripts/check-file-size.sh
# migration 番号の重複は新規 DB でしか壊れない＝ローカルの既存 DB では気づけないため、
# CI に任せず必ずここで検出する。
run_gate "migration version (番号重複)" bash scripts/check-migration-versions.sh

# ---------- Rust ----------
if touches '^(crates/|Cargo\.(toml|lock)$)'; then
  run_gate "cargo fmt --check" cargo fmt --all --check
  run_gate "cargo clippy -D warnings" cargo clippy --all-targets --all-features -- -D warnings
  if command -v cargo-nextest >/dev/null 2>&1; then
    run_gate "cargo nextest" cargo nextest run --workspace --all-features
  else
    run_gate "cargo test" cargo test --workspace --all-features
  fi
  run_gate "cargo machete (未使用依存)" cargo machete crates
fi

if [ "$FAST" = 0 ] && touches '^Cargo\.(toml|lock)$|^crates/.*/Cargo\.toml$'; then
  run_gate "cargo deny check" cargo deny --all-features check
fi

# doctest は nextest が実行しない（CI では coverage 側で走る）。
if touches '^crates/' && [ "$FAST" = 0 ]; then
  run_gate "cargo test --doc" cargo test --workspace --doc
fi

# ---------- tabular runner（workspace 除外クレート） ----------
if [ "$FAST" = 0 ] && touches '^crates/tabular/'; then
  run_gate "tabular runner fmt/clippy" sh_c \
    'cargo fmt --manifest-path crates/tabular/runner/Cargo.toml --check && cargo clippy --manifest-path crates/tabular/runner/Cargo.toml --release -- -D warnings'
  # fmt/clippy だけでは、外部参照拒否・DML/DDL 拒否・クォータ（PIT-39）を壊しても通ってしまう。
  # CI の tabular-runner ジョブと同じく release ビルド ＋ adversarial テストまで回す。
  run_gate "tabular runner build + adversarial" sh_c \
    'cargo build --manifest-path crates/tabular/runner/Cargo.toml --release && SHIKI_TABULAR_RUNNER=crates/tabular/runner/target/release/shiki-tabular-runner cargo test -p shiki-tabular --test runner_adversarial_it'
fi

# ---------- Web ----------
# CI の web ジョブは非 docs PR なら無条件に gen:api → lint → build を回す。web/ の差分だけを
# 条件にすると、Rust 側の OpenAPI/route/DTO だけを変えた PR で codegen 後の型崩れを見逃す
# （Rust テストは通るが web が壊れる）。型の生成元になる Rust 差分も条件に含める。
if touches '^web/' || touches '^crates/'; then
  run_gate "pnpm install" sh_c 'cd web && pnpm install --frozen-lockfile'
  run_gate "pnpm gen:api (codegen が正)" sh_c 'cd web && pnpm gen:api'
  run_gate "pnpm lint" sh_c 'cd web && pnpm lint'
  if [ "$FAST" = 0 ]; then
    # 稼働中の next dev と同じ worktree で build すると .next を壊す。
    if curl -fsS --max-time 2 -o /dev/null http://localhost:3000/ 2>/dev/null; then
      echo "   ⚠️  :3000 で dev サーバが稼働中。pnpm build は .next を壊すためスキップします。"
      echo "      （dev を止めてから改めて回すか、別 worktree でビルドしてください）"
      RESULTS="${RESULTS}⚠️  next build 未実行（:3000 稼働中のためスキップ）\n"
      SKIPPED=1
    else
      # `pnpm build` は package.json の prebuild フックで gen:api を再実行する。直前に
      # 明示実行しているので、ここでは next build を直接呼んで codegen の二重実行を避ける
      # （gen-api.sh は cargo run を 4 本 ＋ openapi-typescript を回すため二重化は高い）。
      run_gate "next build" sh_c 'cd web && pnpm exec next build'
    fi
  fi
fi

# ---------- Python ----------
if touches '^ingestion-worker/'; then
  run_gate "uv sync" sh_c 'cd ingestion-worker && uv sync --frozen'
  run_gate "ruff" sh_c 'cd ingestion-worker && uv run ruff check .'
  [ "$FAST" = 0 ] && run_gate "pytest (fast)" sh_c 'cd ingestion-worker && uv run pytest -m "not slow" -q'
fi

# ---------- カバレッジ注意喚起 ----------
# vendor/ は所有フォークでカバレッジ対象外（ci.yml の ignore-filename-regex で除外）。
NEW_RS=$(git diff --name-only --diff-filter=A "$BASE"...HEAD 2>/dev/null \
  | grep '\.rs$' | grep -v '^vendor/' || true)
if [ -n "$NEW_RS" ]; then
  echo
  echo "📊 新規 .rs が $(printf '%s\n' "$NEW_RS" | wc -l) 件あります。Coverage は workspace 総計 80% の絶対床ゲートです。"
  echo "   テストの無い新規ファイルは総計を 80% 未満へ引きずり落とします:"
  printf '%s\n' "$NEW_RS" | head -20 | sed 's/^/   /'
  [ "$(printf '%s\n' "$NEW_RS" | wc -l)" -gt 20 ] && echo "   ...（以下省略）"
  echo "   → 特に crates/api/src/routes/*.rs は crates/api/tests/<feature>_http.rs を足すこと"
  echo "     （references/gates.md「カバレッジ 80% ゲート」参照）"
fi

# ---------- 結果 ----------
echo
echo "================================"
printf '%b' "$RESULTS"
echo "================================"
if [ "$FAILED" -eq 0 ] && [ "$SKIPPED" -eq 1 ]; then
  # 必須ゲートを飛ばしたまま「全ゲート通過」と言わない（production build でしか出ない
  # エラーを抱えたまま push させてしまう）。
  echo "⚠️  未実行のゲートがあります。上記を実行してから push してください。"
  echo "   （:3000 の dev サーバを止める、または別 worktree で next build する）"
  exit 1
fi
if [ "$FAILED" -eq 0 ]; then
  [ "$FAST" = 1 ] && echo "--fast で通過。push 前にフル（--fast なし）で回すこと。" || echo "全ゲート通過。"
  exit 0
fi
echo "落ちたゲートを直してから push してください。"
exit 1
