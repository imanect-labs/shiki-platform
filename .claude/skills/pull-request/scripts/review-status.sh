#!/usr/bin/env bash
# review-status.sh — PR の品質ゲート状態を1コマンドで判定する。
#
#   使い方: review-status.sh [PR番号]
#     PR番号 省略時は現在のブランチの PR を使う。
#
#   出力: CI チェック状態 ＋ ゲート扱い AI レビュアーの未解消スレッド一覧。
#   exit: 0=緑（checks 全 pass かつ未解消スレッド無し） / 1=ブロック / 2=取得エラー。
#
#   環境変数:
#     PR_REVIEW_BOTS  ゲート扱いする bot ログイン（空白区切り）。
#                     既定: "coderabbitai[bot] cubic-dev-ai[bot]"
set -euo pipefail

PR_REVIEW_BOTS="${PR_REVIEW_BOTS:-coderabbitai[bot] cubic-dev-ai[bot]}"

err() { printf '%s\n' "$*" >&2; }

command -v gh >/dev/null 2>&1 || { err "gh が見つかりません。gh CLI をインストールしてください。"; exit 2; }
command -v jq >/dev/null 2>&1 || { err "jq が見つかりません。jq をインストールしてください。"; exit 2; }
gh auth status >/dev/null 2>&1 || { err "gh が未認証です。'gh auth login' を実行してください。"; exit 2; }

PR="${1:-}"
# PR 番号と owner/repo を解決する。
if ! meta=$(gh pr view ${PR:+"$PR"} --json number 2>/dev/null); then
  err "PR が見つかりません（番号指定か、PR のあるブランチで実行してください）。"
  exit 2
fi
PR_NUM=$(printf '%s' "$meta" | jq -r '.number')

# owner/repo はリポジトリ既定から取得（フォーク差異を避ける）。
if ! repo=$(gh repo view --json owner,name -q '.owner.login + "/" + .name' 2>/dev/null); then
  err "リポジトリ情報を取得できません。"
  exit 2
fi
OWNER="${repo%/*}"
NAME="${repo#*/}"

blocked=0

# --- CI チェック ---
echo "== CI チェック (PR #$PR_NUM) =="
checks_json=$(gh pr checks "$PR_NUM" --json name,state 2>/dev/null || echo '[]')
if [ "$(printf '%s' "$checks_json" | jq 'length')" -eq 0 ]; then
  echo "  （チェックなし）"
else
  printf '%s' "$checks_json" | jq -r '.[] | "  [\(.state)] \(.name)"'
  # SUCCESS / SKIPPED / NEUTRAL 以外が1つでもあればブロック。
  fail=$(printf '%s' "$checks_json" \
    | jq '[.[] | select(.state | ascii_upcase | (. != "SUCCESS" and . != "SKIPPED" and . != "NEUTRAL"))] | length')
  [ "$fail" -gt 0 ] && blocked=1
fi

# --- 未解消 AI レビュースレッド ---
echo "== 未解消 AI レビュースレッド =="
# GraphQL で reviewThreads を取得し、isResolved=false かつ対象 bot のものを抽出。
threads=$(gh api graphql -F owner="$OWNER" -F name="$NAME" -F pr="$PR_NUM" -f query='
  query($owner:String!, $name:String!, $pr:Int!) {
    repository(owner:$owner, name:$name) {
      pullRequest(number:$pr) {
        reviewThreads(first:100) {
          nodes {
            isResolved
            comments(first:1) {
              nodes { author { login } path body }
            }
          }
        }
      }
    }
  }' 2>/dev/null || echo '{}')

bots_json=$(printf '%s' "$PR_REVIEW_BOTS" | jq -R 'split(" ") | map(select(length>0))')

unresolved=$(printf '%s' "$threads" | jq -r --argjson bots "$bots_json" '
  [ .data.repository.pullRequest.reviewThreads.nodes[]?
    | select(.isResolved == false)
    | .comments.nodes[0] as $c
    | select($c.author.login as $a | $bots | index($a))
    | "  [\($c.author.login)] \($c.path // "-"): \(($c.body // "") | gsub("\n"; " ") | .[0:160])"
  ] | .[]' 2>/dev/null || true)

if [ -n "$unresolved" ]; then
  printf '%s\n' "$unresolved"
  blocked=1
else
  echo "  （未解消なし）"
fi

echo "================================"
if [ "$blocked" -eq 0 ]; then
  echo "緑: チェック全 pass・未解消 AI スレッドなし"
  exit 0
fi
echo "ブロック: 上記を解消してください"
exit 1
