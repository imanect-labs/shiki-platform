#!/usr/bin/env bash
# review-status.sh — PR の品質ゲート状態を1コマンドで判定する。
#
#   使い方: review-status.sh [PR番号]
#     PR番号 省略時は現在のブランチの PR を使う。
#
#   出力:
#     1. CI チェック状態
#     2. 未解消の AI レビュースレッド
#     3. 【重要】最終コミットより後に付いた bot コメント/レビュー
#        bot はインラインコメントをスレッド解決済みにしないため、
#        1 と 2 だけでは「対応漏れ」を検出できない。実際に PR #393/#394 で
#        「12/12 pass・未解消スレッドなし」の状態から 7 件が未対応で残り、
#        うち 2 件は実バグだった。時刻の突き合わせが唯一の確実な検出手段。
#     4. スタック PR（base != 既定ブランチ）の警告
#        CodeRabbit は base が既定ブランチ以外の PR を自動レビューしない。
#        `gh pr comment <n> --body "@coderabbitai review"` で明示トリガが要る。
#
#   exit: 0=緑 / 1=ブロック / 2=取得エラー。
#
#   環境変数:
#     PR_REVIEW_BOTS  ゲート扱いする bot ログイン（空白区切り）。
#                     既定: "coderabbitai[bot] chatgpt-codex-connector[bot]"
set -euo pipefail

PR_REVIEW_BOTS="${PR_REVIEW_BOTS:-coderabbitai[bot] chatgpt-codex-connector[bot]}"

err() { printf '%s\n' "$*" >&2; }

command -v gh >/dev/null 2>&1 || { err "gh が見つかりません。gh CLI をインストールしてください。"; exit 2; }
command -v jq >/dev/null 2>&1 || { err "jq が見つかりません。jq をインストールしてください。"; exit 2; }
gh auth status >/dev/null 2>&1 || { err "gh が未認証です。'gh auth login' を実行してください。"; exit 2; }

PR="${1:-}"
if ! meta=$(gh pr view ${PR:+"$PR"} --json number,baseRefName,url 2>/dev/null); then
  err "PR が見つかりません（番号指定か、PR のあるブランチで実行してください）。"
  exit 2
fi
PR_NUM=$(printf '%s' "$meta" | jq -r '.number')
BASE_REF=$(printf '%s' "$meta" | jq -r '.baseRefName')

if ! repo=$(gh repo view --json owner,name,defaultBranchRef -q '.owner.login + "/" + .name + " " + .defaultBranchRef.name' 2>/dev/null); then
  err "リポジトリ情報を取得できません。"
  exit 2
fi
OWNER="${repo%%/*}"
rest="${repo#*/}"
NAME="${rest%% *}"
DEFAULT_BRANCH="${rest##* }"

bots_json=$(printf '%s' "$PR_REVIEW_BOTS" | jq -R 'split(" ") | map(select(length>0))')
blocked=0

# --- 1. CI チェック ---
echo "== CI チェック (PR #$PR_NUM) =="
checks_json=$(gh pr checks "$PR_NUM" --json name,state 2>/dev/null || echo '[]')
if [ "$(printf '%s' "$checks_json" | jq 'length')" -eq 0 ]; then
  # 「チェックが 1 件も無い」には 2 つの意味がある:
  #   (a) paths-ignore（docs/**・**.md・.claude/**）のみの変更 → 正常
  #   (b) CI がまだ check run を登録していない / ワークフローが起動していない → 緑ではない
  # 両者を「チェックなし」で一括りにすると (b) を緑と誤判定するので、変更ファイルで判別する。
  changed=$(gh pr diff "$PR_NUM" --name-only 2>/dev/null || true)
  if [ -z "$changed" ]; then
    echo "  ⚠️  チェックが 1 件も無く、変更ファイルも取得できません。CI の起動を確認してください。"
    blocked=1
  elif printf '%s\n' "$changed" | grep -qvE '^(docs/|\.claude/)|\.md$'; then
    echo "  ⚠️  チェックが 1 件もありません。paths-ignore の対象外ファイルを含むため、CI が"
    echo "     未起動か check run 未登録の可能性があります（push 直後ならしばらく待って再実行）:"
    printf '%s\n' "$changed" | grep -vE '^(docs/|\.claude/)|\.md$' | head -5 | sed 's/^/       /'
    blocked=1
  else
    echo "  （チェックなし — paths-ignore 対象〔docs/**・**.md・.claude/**〕のみの変更のため正常）"
  fi
else
  printf '%s' "$checks_json" | jq -r '.[] | "  [\(.state)] \(.name)"'
  fail=$(printf '%s' "$checks_json" \
    | jq '[.[] | select(.state | ascii_upcase | (. != "SUCCESS" and . != "SKIPPED" and . != "NEUTRAL"))] | length')
  [ "$fail" -gt 0 ] && blocked=1
fi

# --- 2. 未解消 AI レビュースレッド ---
echo "== 未解消 AI レビュースレッド =="
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

# --- 3. 最終コミットより後に付いた bot コメント（取りこぼし検出） ---
echo "== 最終コミット後に付いた bot レビュー =="
last_commit=$(gh api --paginate "repos/$OWNER/$NAME/pulls/$PR_NUM/commits" \
  --jq '.[].commit.committer.date' 2>/dev/null | sort | tail -1 || true)

if [ -z "$last_commit" ]; then
  echo "  （コミット情報を取得できませんでした）"
else
  echo "  最終コミット: $last_commit"

  # インラインレビューコメント。
  # CodeRabbit は修正を確認した後に「対応済み」「指摘を取り下げ」の返信を付ける。これらは
  # 常に最終コミットより後になるため、除外しないと恒久的に赤くなる（対応すべき指摘ではない）。
  ACK_MARKERS='<review_comment_addressed>|<review_comment_withdrawn>'
  # コメント一覧は 1 回だけ取得して使い回す（--paginate は PR あたり複数リクエストになるため、
  # 同じ内容を 2 度引かない）。
  # 取得失敗を空配列に化けさせない。失敗を「コメントなし」と扱うと、取りこぼし検出が
  # 動いていない状態のまま「緑」を返してしまう（レート制限・権限不足・一時障害で起きる）。
  if ! comments_json=$(gh api --paginate "repos/$OWNER/$NAME/pulls/$PR_NUM/comments" 2>/dev/null); then
    err "レビューコメントの取得に失敗しました（レート制限 / 権限 / 一時障害）。判定できません。"
    exit 2
  fi

  late_inline=$(printf '%s' "$comments_json" \
    | jq -r --argjson bots "$bots_json" --arg t "$last_commit" --arg ack "$ACK_MARKERS" '
      [ .[]
        | select(.user.login as $a | $bots | index($a))
        | select(.created_at > $t)
        | select((.body // "") | test($ack) | not)
        | "  [\(.created_at)] [\(.user.login)] id=\(.id) \(.path):\(.line // .original_line // "-")\n      \((.body // "") | gsub("\n"; " ") | .[0:200])"
      ] | .[]' 2>/dev/null || true)

  ack_count=$(printf '%s' "$comments_json" \
    | jq --argjson bots "$bots_json" --arg t "$last_commit" --arg ack "$ACK_MARKERS" '
      [ .[] | select(.user.login as $a | $bots | index($a))
             | select(.created_at > $t)
             | select((.body // "") | test($ack)) ] | length' 2>/dev/null || echo 0)

  # レビュー本体（サマリ本文）。
  if ! reviews_json=$(gh api --paginate "repos/$OWNER/$NAME/pulls/$PR_NUM/reviews" 2>/dev/null); then
    err "レビュー一覧の取得に失敗しました。判定できません。"
    exit 2
  fi
  late_reviews=$(printf '%s' "$reviews_json" \
    | jq -r --argjson bots "$bots_json" --arg t "$last_commit" '
      [ .[]
        | select(.user.login as $a | $bots | index($a))
        | select((.submitted_at // "") > $t)
        | select((.body // "") | length > 0)
        | "  [\(.submitted_at)] [\(.user.login)] review(\(.state))\n      \((.body // "") | gsub("\n"; " ") | .[0:200])"
      ] | .[]' 2>/dev/null || true)

  # サマリレビューのうちブロックすべきなのは CHANGES_REQUESTED だけ。COMMENTED の
  # サマリ（「指摘なし」を含む）で止めると、再 push で再レビューされるたびにサマリが
  # 最終コミットより後になり、完了条件に永久に到達できない。
  blocking_reviews=$(printf '%s' "$reviews_json" \
    | jq -r --argjson bots "$bots_json" --arg t "$last_commit" '
      [ .[]
        | select(.user.login as $a | $bots | index($a))
        | select((.submitted_at // "") > $t)
        | select(.state == "CHANGES_REQUESTED")
      ] | length' 2>/dev/null || echo 0)

  [ -n "$late_reviews" ] && { printf '%s\n' "$late_reviews"; echo "  （↑ サマリ。ブロック対象はインライン指摘と CHANGES_REQUESTED のみ）"; }
  if [ -n "$late_inline" ] || [ "${blocking_reviews:-0}" -gt 0 ]; then
    [ -n "$late_inline" ] && printf '%s\n' "$late_inline"
    echo "  ⚠️  最終コミットより後の指摘です。未対応の可能性が高い。"
    echo "     返信: gh api repos/$OWNER/$NAME/pulls/$PR_NUM/comments/<id>/replies -f body=\"...\""
    echo "     （単体取得は /repos/$OWNER/$NAME/pulls/comments/<id> — /pulls/$PR_NUM/comments/<id> は 404）"
    blocked=1
  else
    echo "  （なし）"
  fi
  [ "${ack_count:-0}" -gt 0 ] && echo "  （うち $ack_count 件は bot の「対応済み/取り下げ」返信のため除外）"

  # --- 3.5 bot が「最新コミットを見たか」を確認する ---
  # 「最終コミット後のコメントが無い」には 2 つの意味がある:
  #   (a) 指摘が全て対応済み → 緑
  #   (b) bot がまだ最新コミットをレビューしていない → 緑ではない（見ていないだけ）
  # 時刻比較だけでは (b) を緑と誤判定する。bot が実際にどの commit をレビューしたかで判別する。
  head_sha=$(gh api --paginate "repos/$OWNER/$NAME/pulls/$PR_NUM/commits" --jq '.[-1].sha' 2>/dev/null || true)
  if [ -n "$head_sha" ]; then
    pending_bots=""
    for bot in $PR_REVIEW_BOTS; do
      reviewed=$(printf '%s' "$reviews_json" | jq -r --arg b "$bot" --arg sha "$head_sha" \
        '[ .[] | select(.user.login == $b) | select((.commit_id // "") == $sha) ] | length' 2>/dev/null || echo 0)
      [ "${reviewed:-0}" -eq 0 ] && pending_bots="$pending_bots $bot"
    done
    if [ -n "$pending_bots" ]; then
      echo "  ⏳ 最新コミット（${head_sha:0:8}）を**まだレビューしていない** bot:$pending_bots"
      echo "     「指摘なし」ではなく「未レビュー」です。完了を待ってから再実行してください。"
      blocked=1
    fi
  fi
fi

# --- 4. スタック PR の警告 ---
if [ "$BASE_REF" != "$DEFAULT_BRANCH" ]; then
  echo "== スタック PR =="
  echo "  base = $BASE_REF（既定ブランチ $DEFAULT_BRANCH ではない）"
  echo "  ⚠️  CodeRabbit は base が既定ブランチ以外の PR を自動レビューしません。"
  echo "     明示トリガ: gh pr comment $PR_NUM --body \"@coderabbitai review\""
  echo "     force-push で HEAD が変わったら再トリガすること。"
  # 警告のみ（ブロックはしない）。
fi

echo "================================"
if [ "$blocked" -eq 0 ]; then
  echo "緑: チェック全 pass・未解消スレッドなし・最終コミット後の未対応レビューなし"
  echo "※ 初回は生コメントも一読すること:"
  echo "   gh api repos/$OWNER/$NAME/pulls/$PR_NUM/comments --jq '.[] | \"\\(.created_at) [\\(.user.login)] \\(.path) \\(.body[0:200])\"'"
  exit 0
fi
echo "ブロック: 上記を解消してください"
exit 1
