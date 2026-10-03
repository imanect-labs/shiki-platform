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
if ! meta=$(gh pr view ${PR:+"$PR"} --json number,baseRefName,url,headRefOid,mergeable 2>/dev/null); then
  err "PR が見つかりません（番号指定か、PR のあるブランチで実行してください）。"
  exit 2
fi
PR_NUM=$(printf '%s' "$meta" | jq -r '.number')
BASE_REF=$(printf '%s' "$meta" | jq -r '.baseRefName')
# HEAD の SHA は PR オブジェクトから取る。`gh api --paginate .../commits --jq '.[-1].sha'` は
# jq を**ページごとに**適用するため、コミットが 31 件以上あると SHA が複数行になり、
# 単一の commit_id と永久に一致せず「レビュー済みの bot も未レビュー」と誤判定する。
HEAD_SHA=$(printf '%s' "$meta" | jq -r '.headRefOid // ""')
# CONFLICTING の間、GitHub はマージ ref を作れず **`pull_request` の run を一切生成しない**。
# この状態は「チェックが 0 件」として現れるため、paths-ignore による 0 件と区別が付かない。
# 区別しないと「docs のみだからチェック無しで正常」と読んでしまい、CI が一度も走っていない
# PR を緑と報告する（実際に踏んだ）。待っても解消しないので、待機ではなく rebase を促す。
MERGEABLE=$(printf '%s' "$meta" | jq -r '.mergeable // "UNKNOWN"')

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

# --- 0. マージ可能性 ---
# チェックより先に見る。CONFLICTING なら CI がそもそも走らないため、
# チェックの読み方（0 件の意味）が変わる。
if [ "$MERGEABLE" = "CONFLICTING" ]; then
  echo "== マージ可能性 =="
  echo "  ❌ base ($BASE_REF) とコンフリクトしています。"
  echo "     この状態では GitHub がマージ ref を作れず、pull_request の CI は起動しません。"
  echo "     待っても解消しません。rebase してから再実行してください:"
  echo "       git fetch origin && git rebase origin/$BASE_REF"
  echo
  blocked=1
fi

# --- 1. CI チェック ---
echo "== CI チェック (PR #$PR_NUM) =="
checks_json=$(gh pr checks "$PR_NUM" --json name,state 2>/dev/null || echo '[]')
if [ "$(printf '%s' "$checks_json" | jq 'length')" -eq 0 ]; then
  # 「チェックが 1 件も無い」には 3 つの意味がある:
  #   (a) paths-ignore（docs/**・**.md・.claude/**）のみの変更 → 正常
  #   (b) CI がまだ check run を登録していない / ワークフローが起動していない → 緑ではない
  #   (c) PR がコンフリクトしていて run が生成されない → 緑ではない（上の 0. で検出済み）
  # 一括りにすると (b)(c) を緑と誤判定するので、変更ファイルとマージ可能性で判別する。
  changed=$(gh pr diff "$PR_NUM" --name-only 2>/dev/null || true)
  if [ "$MERGEABLE" = "CONFLICTING" ]; then
    echo "  （チェックなし — 上記のコンフリクトにより CI が起動していません）"
  elif [ -z "$changed" ]; then
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
# ここは**コミット時刻に関係なく**未解決スレッドを全部出す。3. の「最終コミット後の
# コメント」だけに頼ると、コミットを重ねた時に古い未対応コメントが判定から外れる。
echo "== 未解消 AI レビュースレッド =="
# ⚠️ `first: N` は最初のページしか返さない。ページングしないと 101 件目以降の未対応スレッドを
#    見落とし（＝偽の緑）、スレッド内の返信も打ち切られて「返信済みなのに未返信」と誤ってブロックする。
#    reviewThreads は `--paginate`（$endCursor 変数を宣言すると gh が自動で辿る）で全件取る。
#    スレッド内の comments は入れ子なので --paginate の対象外。上限に達したら黙って切り詰めず、
#    fail-closed で exit 2 にする（100 件超の返信が付くスレッドは異常なので、緑を返すより止める）。
if ! threads_raw=$(gh api graphql --paginate \
  -F owner="$OWNER" -F name="$NAME" -F pr="$PR_NUM" -f query='
  query($owner:String!, $name:String!, $pr:Int!, $endCursor:String) {
    repository(owner:$owner, name:$name) {
      pullRequest(number:$pr) {
        reviewThreads(first:100, after:$endCursor) {
          pageInfo { hasNextPage endCursor }
          nodes {
            isResolved
            comments(first:100) {
              pageInfo { hasNextPage }
              nodes { author { login } path line originalLine body }
            }
          }
        }
      }
    }
  }' 2>&1); then
  # ⚠️ 取得失敗を空結果に化かさない（`|| echo '{}'` にすると「スレッド無し」＝緑になる）。
  err "レビュースレッドを取得できませんでした:"
  printf '%s\n' "$threads_raw" | head -5 >&2
  exit 2
fi

# --paginate はページごとに JSON を 1 個ずつ出すので、slurp して nodes を連結する。
if ! threads=$(printf '%s' "$threads_raw" \
  | jq -s '[ .[].data.repository.pullRequest.reviewThreads.nodes[]? ]' 2>&1); then
  err "レビュースレッドの解析に失敗しました:"
  printf '%s\n' "$threads" | head -5 >&2
  exit 2
fi

truncated=$(printf '%s' "$threads" | jq '[ .[] | select(.comments.pageInfo.hasNextPage) ] | length' 2>/dev/null || echo "?")
if [ "$truncated" != "0" ]; then
  err "スレッド内のコメントが上限を超えました（${truncated} 件のスレッドで打ち切り）。判定できません。"
  err "  review-status.sh の comments(first:100) を増やすか、ページングを実装してください。"
  exit 2
fi

# ⚠️ bot ログインの表記が API で違う。REST は `coderabbitai[bot]`、**GraphQL は
#    `coderabbitai`**（`[bot]` が付かない）。PR_REVIEW_BOTS は REST 表記なので、
#    そのまま突合すると一致せず、この節が**恒久的に「未解消なし」を返す**。
#    実際に本スクリプトはこの不一致で、未解決スレッドが 3 件ある PR を緑と報告していた。
#    比較前に両側の `[bot]` を落として正規化する。
# ブロックするのは「未解決 **かつ** 人間の返信が 1 件も無い」スレッドだけにする。
# Codex はスレッドを解決しないため、`isResolved == false` だけでブロックすると
# 根拠を返信しても永久に緑にならない（恒久ブロック＝ゲートとして使い物にならなくなる）。
# 返信済みのものは件数だけ出して人の目に残す。
filter='
  ($bots | map(sub("\\[bot\\]$"; ""))) as $names
  | [ .[]?
      | select(.isResolved == false)
      | . as $t
      | $t.comments.nodes[0] as $c
      | select(($c.author.login // "" | sub("\\[bot\\]$"; "")) as $a | $names | index($a))
      | ( [ $t.comments.nodes[]
            | select((.author.login // "" | sub("\\[bot\\]$"; "")) as $a | ($names | index($a)) | not) ]
          | length ) as $human_replies
      | select(($human_replies > 0) == ANSWERED)
      | "  [\($c.author.login)] \($c.path // "-"):\($c.line // $c.originalLine // "-")\n      \(($c.body // "") | gsub("\n"; " ") | .[0:150])"
    ]'

# ⚠️ jq の失敗を握り潰さない。`2>/dev/null || true` にすると、フィルタの構文誤りや想定外の
#    JSON 形状で unaddressed が空・answered が 0 になり、**判定していないのに緑**になる。
if ! unaddressed=$(printf '%s' "$threads" | jq -r --argjson bots "$bots_json" "${filter//ANSWERED/false} | .[]" 2>&1); then
  err "未対応スレッドの判定に失敗しました（jq）:"
  printf '%s\n' "$unaddressed" | head -5 >&2
  exit 2
fi
if ! answered=$(printf '%s' "$threads" | jq -r --argjson bots "$bots_json" "${filter//ANSWERED/true} | length" 2>&1); then
  err "返信済みスレッドの集計に失敗しました（jq）:"
  printf '%s\n' "$answered" | head -5 >&2
  exit 2
fi

if [ -n "$unaddressed" ]; then
  printf '%s\n' "$unaddressed"
  echo "  → 対応するか、根拠を返信すること:"
  echo "     gh api repos/$OWNER/$NAME/pulls/$PR_NUM/comments/<id>/replies -f body=\"...\""
  blocked=1
else
  echo "  （未返信のスレッドなし）"
fi
if [ "${answered:-0}" -gt 0 ] 2>/dev/null; then
  echo "  ℹ️  返信済みだが未解決のスレッドが ${answered} 件あります（ブロックしません）。"
  echo "     bot がスレッドを解決しないだけのことが多いが、返信で議論が終わっているか一度は確認すること。"
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

  # ⚠️ スレッドへの**返信**（`in_reply_to_id` あり）は新規指摘ではない。既存スレッドの続きなので
  #    2. の未解消スレッド判定が担当する。ここで拾うと、CodeRabbit の
  #    「修正を確認しました／この指摘は解消されています」という返信を新規指摘として数えて
  #    恒久的に赤くなる（ACK_MARKERS は付かないことがあるので、マーカー除外だけでは防げない）。
  #    実例: CodeRabbit は「スレッドを解決できなかったので open のまま。手動で解決して」と
  #    返信することがあり、この文面にマーカーは無い。
  late_inline=$(printf '%s' "$comments_json" \
    | jq -r --argjson bots "$bots_json" --arg t "$last_commit" --arg ack "$ACK_MARKERS" '
      [ .[]
        | select(.user.login as $a | $bots | index($a))
        | select(.in_reply_to_id == null)
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
  #   (b) bot がまだ最新コミットを見ていない → 緑ではない（見ていないだけ）
  # 時刻比較だけでは (b) を緑と誤判定する。
  #
  # ただし「review オブジェクトが無い＝未レビュー」ではない:
  #   - CodeRabbit は指摘ゼロだと review を作らず、commit status（context="CodeRabbit"）だけ出す。
  #   - Codex は push ごとの再レビューをしない（初回のみのことがある）。
  # したがって review の有無だけでブロックすると、恒久的に緑にならなくなる。
  # ブロックするのは「レビューが実行中（pending）」＝結果が未確定の時だけにして、
  # 「そもそも見ていない」は警告に留める（判断材料は出すが停止させない）。
  if [ -n "$HEAD_SHA" ]; then
    # HEAD に対する pending なチェック/ステータスがあるか（レビュー実行中）。
    head_pending=$(gh api "repos/$OWNER/$NAME/commits/$HEAD_SHA/status" \
      --jq '[.statuses[]? | select(.state == "pending")] | length' 2>/dev/null || echo 0)
    head_pending_runs=$(gh api "repos/$OWNER/$NAME/commits/$HEAD_SHA/check-runs" \
      --jq '[.check_runs[]? | select(.status != "completed")] | length' 2>/dev/null || echo 0)
    if [ "${head_pending:-0}" -gt 0 ] || [ "${head_pending_runs:-0}" -gt 0 ]; then
      echo "  ⏳ 最新コミット（${HEAD_SHA:0:8}）のレビュー/チェックが実行中です。結果は未確定。"
      blocked=1
    fi

    # 参考情報: HEAD に対して review も status も残していない bot を挙げる。
    silent_bots=""
    for bot in $PR_REVIEW_BOTS; do
      reviewed=$(printf '%s' "$reviews_json" | jq -r --arg b "$bot" --arg sha "$HEAD_SHA" \
        '[ .[] | select(.user.login == $b) | select((.commit_id // "") == $sha) ] | length' 2>/dev/null || echo 0)
      [ "${reviewed:-0}" -eq 0 ] && silent_bots="$silent_bots $bot"
    done
    if [ -n "$silent_bots" ]; then
      echo "  ℹ️  最新コミット（${HEAD_SHA:0:8}）に review を残していない bot:$silent_bots"
      echo "     指摘ゼロで review を作らない場合と、再レビューしていない場合がある。"
      echo "     判断に使うなら PR ページで最終レビュー対象コミットを確認すること（ブロックはしない）。"
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
