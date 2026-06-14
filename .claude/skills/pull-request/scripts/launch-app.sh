#!/usr/bin/env bash
# launch-app.sh — 変更を検証するため 0.0.0.0:{10000+issue番号} でアプリを起動する。
#
#   使い方: launch-app.sh <issue番号> [web|api|both]
#
#   PORT = 10000 + issue番号。0.0.0.0 にバインドして起動 URL を表示する。
#   target 省略時は git diff から判定する:
#     web/                       -> web (Next.js)
#     crates/ , ingestion-worker -> api (axum)
#     両方                       -> both（web=PORT, api=PORT+1）
#
#   実装が未着手（web/ も crates/api も無い設計フェーズ）なら、起動せず
#   スキップを表示して正常終了する。
#
#   注意: バイナリ名（cargo run -p api）/ web の起動コマンドは現行の規約準拠。
#   実装着手後に実バイナリ名・pnpm スクリプトへ要追従（TODO）。
set -euo pipefail

err() { printf '%s\n' "$*" >&2; }

ISSUE="${1:-}"
TARGET="${2:-auto}"

case "$ISSUE" in
  ''|*[!0-9]*) err "使い方: launch-app.sh <issue番号> [web|api|both]"; exit 2 ;;
esac

PORT=$((10000 + ISSUE))

# リポジトリルートへ移動。
ROOT=$(git rev-parse --show-toplevel 2>/dev/null) || { err "git リポジトリ内で実行してください。"; exit 2; }
cd "$ROOT"

HAS_WEB=0; [ -f web/package.json ] && HAS_WEB=1
HAS_API=0; { [ -d crates/api ] || [ -d ingestion-worker ]; } && HAS_API=1

# --- target 自動判定 ---
if [ "$TARGET" = "auto" ]; then
  BASE=$(git symbolic-ref --short refs/remotes/origin/HEAD 2>/dev/null | sed 's@^origin/@@') || true
  BASE=${BASE:-main}
  CHANGED=$(git diff --name-only "$BASE"...HEAD 2>/dev/null || git diff --name-only)
  HIT_WEB=0; printf '%s\n' "$CHANGED" | grep -q '^web/' && HIT_WEB=1
  HIT_API=0; printf '%s\n' "$CHANGED" | grep -Eq '^(crates/|ingestion-worker/)' && HIT_API=1
  if [ "$HIT_WEB" = 1 ] && [ "$HIT_API" = 1 ]; then TARGET=both
  elif [ "$HIT_API" = 1 ]; then TARGET=api
  elif [ "$HIT_WEB" = 1 ]; then TARGET=web
  else TARGET=${HAS_WEB:+web}; TARGET=${TARGET:-api}  # 既定: web があれば web
  fi
  echo "自動判定 target: $TARGET（base=$BASE）"
fi

start_api() {
  local port="$1"
  if [ "$HAS_API" != 1 ]; then
    echo "（api 未実装のため起動スキップ）"
    return 0
  fi
  echo "axum を起動: http://0.0.0.0:$port  (/healthz で生存確認)"
  # TODO: 実バイナリ名に追従。.env.example の AXUM_BIND_ADDR 規約に合わせる。
  AXUM_BIND_ADDR="0.0.0.0:$port" cargo run -p api
}

start_web() {
  local port="$1"
  if [ "$HAS_WEB" != 1 ]; then
    echo "（web 未実装のため起動スキップ）"
    return 0
  fi
  echo "Next.js を起動: http://0.0.0.0:$port"
  ( cd web && pnpm exec next dev -H 0.0.0.0 -p "$port" )
}

if [ "$HAS_WEB" != 1 ] && [ "$HAS_API" != 1 ]; then
  echo "web/ も crates/api も未実装（設計フェーズ）。起動をスキップします。"
  echo "実装着手後、PORT=$PORT（=10000+$ISSUE）で 0.0.0.0 起動できます。"
  exit 0
fi

case "$TARGET" in
  web)  start_web "$PORT" ;;
  api)  start_api "$PORT" ;;
  both)
    echo "both: web=$PORT, api=$((PORT+1))"
    start_api "$((PORT+1))" &
    API_PID=$!
    trap 'kill "$API_PID" 2>/dev/null || true' EXIT INT TERM
    start_web "$PORT"
    ;;
  *) err "未知の target: $TARGET（web|api|both）"; exit 2 ;;
esac
