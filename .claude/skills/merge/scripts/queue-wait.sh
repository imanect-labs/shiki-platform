#!/usr/bin/env bash
# queue-wait.sh — マージキューに入れた PR が main に入るまで待ち、結果を exit code で返す。
#
#   使い方: queue-wait.sh <PR番号> [タイムアウト分（既定 120）]
#
#   exit: 0=マージ済み / 1=キューから外された（キューの CI 失敗・コンフリクト等）
#         2=PR がクローズされた・取得エラー・引数誤り / 3=タイムアウト
#
#   `gh pr view --json state` だけでは「キュー待ち」と「キューから外されて OPEN に戻った」を区別できない。
#   GraphQL の mergeQueueEntry（キュー内なら非 null）と autoMergeRequest（必須チェック待ちで
#   auto-merge を予約中なら非 null・まだキューには居ない）で判定する。
#
#   環境変数:
#     QUEUE_WAIT_INTERVAL  ポーリング間隔秒（既定 60）
set -euo pipefail

err() { printf '%s\n' "$*" >&2; }

# 引数誤りを exit 1（＝キューから外された）に化けさせない。set -e に任せると 1 で落ちる。
PR="${1:-}"
TIMEOUT_MIN="${2:-120}"
INTERVAL="${QUEUE_WAIT_INTERVAL:-60}"
for v in "$PR" "$TIMEOUT_MIN" "$INTERVAL"; do
  case "$v" in ''|*[!0-9]*) err "使い方: queue-wait.sh <PR番号> [タイムアウト分]（いずれも整数）"; exit 2 ;; esac
done

command -v gh >/dev/null 2>&1 || { err "gh が見つかりません。"; exit 2; }
command -v jq >/dev/null 2>&1 || { err "jq が見つかりません。"; exit 2; }

if ! repo=$(gh repo view --json owner,name -q '.owner.login + "/" + .name' 2>/dev/null); then
  err "リポジトリ情報を取得できません。"
  exit 2
fi
OWNER="${repo%%/*}"
NAME="${repo#*/}"

deadline=$(( $(date +%s) + TIMEOUT_MIN * 60 ))
# キューに入った直後は mergeQueueEntry の反映が遅れることがある。
# 一度もキュー内を観測しないまま「外された」と判定しないよう、観測したかを覚えておく。
seen_in_queue=0
misses=0
# 一時的な API エラー（502・瞬間的なレート制限）1 回で見届けを打ち切らない。
api_failures=0
MAX_API_FAILURES=5

while :; do
  if ! res=$(gh api graphql -F owner="$OWNER" -F name="$NAME" -F pr="$PR" -f query='
    query($owner:String!, $name:String!, $pr:Int!) {
      repository(owner:$owner, name:$name) {
        pullRequest(number:$pr) {
          state
          mergedAt
          mergeCommit { oid }
          mergeQueueEntry { state position }
          autoMergeRequest { enabledAt }
        }
      }
    }' 2>/dev/null); then
    api_failures=$((api_failures + 1))
    if [ "$api_failures" -ge "$MAX_API_FAILURES" ]; then
      err "PR 状態の取得に $api_failures 回連続で失敗しました（レート制限 / 権限 / 障害）。"
      exit 2
    fi
    err "PR 状態の取得に失敗（$api_failures/$MAX_API_FAILURES）。再試行します。"
    sleep "$INTERVAL"
    continue
  fi
  api_failures=0

  state=$(printf '%s' "$res" | jq -r '.data.repository.pullRequest.state // "UNKNOWN"')
  entry=$(printf '%s' "$res" | jq -c '.data.repository.pullRequest.mergeQueueEntry')
  auto=$(printf '%s' "$res" | jq -c '.data.repository.pullRequest.autoMergeRequest')
  now=$(date -u +%H:%M:%S)

  case "$state" in
    MERGED)
      oid=$(printf '%s' "$res" | jq -r '.data.repository.pullRequest.mergeCommit.oid // "-"')
      at=$(printf '%s' "$res" | jq -r '.data.repository.pullRequest.mergedAt // "-"')
      echo "[$now] ✅ マージ済み: merge commit $oid（$at）"
      exit 0
      ;;
    CLOSED)
      echo "[$now] ❌ PR がマージされずにクローズされました。"
      exit 2
      ;;
    OPEN)
      if [ "$entry" != "null" ]; then
        seen_in_queue=1
        misses=0
        echo "[$now] ⏳ キュー内: $(printf '%s' "$entry" | jq -r '"\(.state)（位置 \(.position)）"')"
      elif [ "$auto" != "null" ]; then
        # 必須チェック待ちの auto-merge 予約。チェックが緑になればキューに入るので待つ。
        misses=0
        echo "[$now] ⏳ auto-merge 予約中（必須チェックの完了待ち・まだキュー外）"
      else
        misses=$((misses + 1))
        # マージ直後は entry が消えてから state が MERGED になるまでの隙間がありうるので、1 回では判定しない。
        # 観測済みなら 2 回、未観測なら 3 回連続で外れていたら「キューに居ない」と判定する。
        if { [ "$seen_in_queue" -eq 1 ] && [ "$misses" -ge 2 ]; } || [ "$misses" -ge 3 ]; then
          echo "[$now] ❌ PR はキューにも auto-merge 予約にもありません（キューの CI 失敗・コンフリクト・手動除外・"
          echo "     予約中の必須チェック失敗のいずれか）。"
          echo "     キューの run: gh run list --event merge_group --limit 5"
          exit 1
        fi
        echo "[$now] … キュー登録の反映待ち（$misses）"
      fi
      ;;
    *)
      err "想定外の状態: $state"
      exit 2
      ;;
  esac

  if [ "$(date +%s)" -ge "$deadline" ]; then
    echo "[$now] ⌛ ${TIMEOUT_MIN} 分待ってもマージされませんでした（キューには残っています）。"
    exit 3
  fi
  sleep "$INTERVAL"
done
