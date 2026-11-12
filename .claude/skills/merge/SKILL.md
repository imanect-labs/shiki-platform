---
name: merge
description: 作業ツリーの変更を /pr で merge-ready な PR にし、CI と AI レビューが緑になったらマージキューに入れて main に入るまで見届ける。「マージまでやって」「緑になったらマージ」「/merge」の時に使う。PR を作るだけでマージしないなら /pr を使う。
---

# merge: /pr → 緑 → マージキュー → main 反映

```text
Step 1  /pr を回す            Phase 1〜6。緑（review-status.sh が exit 0）になるまで
Step 2  マージ前の確認        緑にした HEAD のまま・base が main・コンフリクトなし・draft でない
Step 3  マージする          キューの有無を確かめ、HEAD の SHA を固定して gh pr merge
Step 4  キューを見届ける      scripts/queue-wait.sh。落ちたら Step 1 の Phase 6 に戻る
Step 5  後始末と報告          Issue のクローズ確認・作った Issue と PR の報告
```

**`/merge` の呼び出しが「この PR をマージしてよい」という human の明示的な依頼になる。**
対象はこの作業の PR 1 本だけ。スタックの親 PR・他人の PR・別タスクの PR は、マージ前に必ず human に確認する。

## やってはいけないこと（どの Step でも）

- 緑でない PR をマージしない。`gh pr merge --admin`・保護ルールの迂回・required check の手動 pass は使わない。
- 緑にするために、テストを skip / 削除 / 緩和しない。bot の指摘を返信も修正もせずにスレッド resolve しない。
- キューで落ちた PR を、原因を見ずに再投入しない（flaky と判断して再投入するのは 1 回まで。判断の根拠を PR に残す）。
- 緑にした後に push したコミットを、ゲートを回し直さずにマージしない。

## Step 1 — /pr を回す

`pr` スキルを Phase 1 から Phase 6 まで、そのスキルの手順どおりに実施する（ここに手順を重複させない）。
既に PR があるなら Phase 6（レビュー解消ループ）から入ってよいが、未実施の Phase があるなら先に回す。

- `/pr` の Phase 7 で**エスカレート**（3 反復で未解消）になったら、**マージせずに止めて** human に報告する。

緑の判定は、**判定した HEAD** を挟み込んで確定させる。判定後に SHA を取ると、その間の push で
検証していない HEAD を `<GREEN_SHA>` として控えてしまい、`--match-head-commit` もそれを素通しする
（このフラグは「その SHA と一致するか」しか見ず、「その SHA が検証済みか」は保証しない）:

```bash
gh pr view --json headRefOid -q .headRefOid        # ① 判定前の HEAD を控える
.claude/skills/pr/scripts/review-status.sh         # ② exit 0 を確認
gh pr view --json headRefOid -q .headRefOid        # ③ ① と一致することを確認
```

① と ③ が一致し、② が exit 0 のときだけ、その SHA を `<GREEN_SHA>` として Step 2 へ進む。不一致なら判定をやり直す。

SHA は**値として**控える。シェル変数に入れて済ませない。Bash ツールは呼び出しごとにシェル状態が消えるので、後の Step で
`$GREEN_SHA` が空になり、`--match-head-commit ""` は**固定なし**として通ってしまう。以降の `<GREEN_SHA>` には控えた 40 桁をそのまま書く。

## Step 2 — マージ前の確認

```bash
gh pr view --json number,url,state,isDraft,baseRefName,mergeable,headRefOid,closingIssuesReferences
```

| 項目 | 期待値 | 違ったら |
| --- | --- | --- |
| `state` | `OPEN` | 止めて報告 |
| `isDraft` | `false` | draft のまま出したのが意図的か human に確認する（勝手に ready にしない） |
| `baseRefName` | `main` | スタック PR。親 PR が main に入り、この PR の base が main に付け替わるまでマージしない。親のマージは human に確認する |
| `mergeable` | `MERGEABLE` | `CONFLICTING` なら `git fetch origin main && git rebase origin/main` → push → **Step 1 に戻る**（HEAD が変わったので緑はやり直し）。`UNKNOWN` は数十秒待って再取得 |
| `headRefOid` | 控えた `<GREEN_SHA>` と一致 | 緑の後に push がある。Step 1 に戻る |
| `closingIssuesReferences` | 対応 Issue がある | 本文に `Closes #<n>` が無ければ足す（`dev-workflow` の規約） |

## Step 3 — マージする（キューがあればキューに入れる）

**先に main にマージキューの規則が実際に掛かっているかを確かめる。** CI（`ci.yml`）は `merge_group` を受けるが、
リポジトリ設定でキューが有効とは限らない（2026-11 時点では `merge_group` の run が 0 件で、PR は squash で直接マージされていた）:

```bash
gh api repos/{owner}/{repo}/rules/branches/main --jq '[.[] | select(.type == "merge_queue")] | length'
```

- **1 以上（キューあり）** — マージ方式はキュー側の設定で決まるので、`--squash` 等は付けない:

  ```bash
  gh pr merge <PR#> --match-head-commit <GREEN_SHA>
  ```

  必須チェックの一部が未完了だと auto-merge の予約になる。それで構わない（`queue-wait.sh` は予約中を待機として扱う）。

- **0（キューなし）** — main の履歴に合わせて squash で直接マージする:

  ```bash
  gh pr merge <PR#> --squash --match-head-commit <GREEN_SHA>
  ```

  キューの全量テストが走らないので、PR で選ばれなかったテストは main の push run で初めて回る。そこで落ちたら
  `selection-gap` が Issue を起票する（Step 5 で main の run も見る）。

どちらでも `--match-head-commit` で、確認した HEAD 以外がマージされるのを防ぐ（その間に誰かが push していたら失敗する → Step 1 へ）。
`--admin` は付けない。保護ルールに弾かれたら、理由を human に報告する。

## Step 4 — キューを見届ける

キューなしで直接マージした場合も `queue-wait.sh` で `MERGED` を確かめる（即 exit 0 になる）。

マージキュー（`merge_group`）は docs 以外の変更を含む限り**全量**のテストを回す。PR では選ばれなかったテストがここで落ちうる
（`.claude/skills/pr/references/gates.md` 冒頭）。

```bash
.claude/skills/merge/scripts/queue-wait.sh <PR#>        # 既定タイムアウト 120 分
```

長く待つので、バックグラウンド実行して完了通知を待ってよい（`sleep` でのポーリングをしない）。

| exit | 意味 | 次の行動 |
| --- | --- | --- |
| `0` | main にマージ済み | Step 5 |
| `1` | キューから外された | 下記「キューで落ちたら」 |
| `2` | クローズ・取得エラー | 状況を確かめて報告（勝手に reopen しない） |
| `3` | タイムアウト | `gh run list --event merge_group --limit 5` で run が動いているか確認。動いていれば再度待つ。詰まっていれば報告 |

### キューで落ちたら

1. 落ちた run を特定してログを読む:

   ```bash
   gh run list --event merge_group --limit 5
   gh run view <run-id> --log-failed
   ```

2. 原因を分類する:
   PR で選ばれなかったテストがキューで落ちると、CI の `selection-gap` ジョブが Issue を自動起票する（既存があればコメント。
   `.claude/skills/pr/references/gates.md`）。**自分で Issue を切る前に、その Issue が出ていないか確認し、あればそれに紐づける**（二重起票しない）。

   - **この PR が壊した**（PR では選ばれなかったテストが落ちた）→ ブランチで直し、`/pr` の Phase 1 から回し直して Step 1 に戻る。
     テスト選択の漏れなら `ci/scenarios.toml` / `scripts/ci-select.py` の規則も同じ PR で直す。
   - **flaky・基盤障害**（この PR の変更と無関係なテスト、再実行で通る）→ 根拠を PR にコメントで残し、Step 3 から 1 回だけ再投入する。
     2 回目も落ちたら flaky と決めつけずに human に報告する。flaky なテストは（selection-gap の Issue が無ければ）Issue に切る。
   - **キュー内の前の PR とのコンフリクト** → rebase して Step 1 に戻る。
3. キュー起因のやり直しは **2 回まで**。超えたら止めて、落ちたジョブ・試したこと・必要な判断を human に報告する。

## Step 5 — 後始末と報告

1. 対応 Issue が閉じたか確かめる（`Closes #<n>` で自動クローズされる）:

   ```bash
   gh issue view <n> --json state,stateReason
   ```

   開いたままなら、マージした PR を書いた完了コメントを付けてクローズする（`--reason completed`）。

2. **キューなしでマージしたなら、main の push run（全量テスト）の結果まで見る。** 対象は**この PR の merge commit の run** に
   限る（最新の数件を見るだけだと、別の push の run を取り違える）。merge commit の SHA は `queue-wait.sh` の出力
   （`merge commit <sha>`）にある:

   ```bash
   gh run list --workflow ci.yml --commit <merge-sha> --event push --json databaseId,status,conclusion
   gh run watch <run-id> --exit-status        # 一覧に出るまで少し待つことがある
   ```

   - **run が作られない変更がある。** main の push には `paths-ignore`（`docs/**`・`*.md`・`.claude/**`）が掛かっている
     （正は `ci.yml` 冒頭）。PR の変更ファイル（`gh pr view <PR#> --json files`）が**すべて**これに当たるなら run は来ないので、
     待たずに「push run なし（paths-ignore）」と報告して次へ進む。
   - **`cancelled` で終わったら、全量テストの結果は得られていない。** main の concurrency は保留中の run を 1 本しか保たず、
     連続 push で保留中の run が取り消される。`gh run rerun <run-id>` で 1 回だけ再実行し、それも取れなければ
     「この merge commit の全量テストは未確認」と報告する（緑とは書かない）。
   - **落ちたら** `selection-gap` の Issue を確認し、この PR が原因なら直す PR を出す。

3. ローカルを最新化する（次のタスクは `origin/main` から切る）:

   ```bash
   git fetch origin main
   ```

   リモートブランチの削除はリポジトリの自動削除設定に任せる。手で消すのは human に頼まれた時だけ。

4. human に報告する:
   - PR の URL と変更の一行要約、merge commit の SHA
   - 検証内容（`/pr` の Phase 2 の結果。UI ならスクリーンショット）
   - キューで落ちてやり直したなら、その原因と対応
   - この作業中に作った Issue（番号・題名・URL）と、クローズした Issue
   - `/pr` Phase 7 のブログ価値の提案（基準を満たす時だけ）

## リファレンス

| 用途 | コマンド |
| --- | --- |
| ゲート判定（緑か） | `.claude/skills/pr/scripts/review-status.sh [PR#]` |
| キューの有無 | `gh api repos/{owner}/{repo}/rules/branches/main --jq '[.[] \| select(.type == "merge_queue")] \| length'` |
| マージ（キューあり / なし） | `gh pr merge <PR#> --match-head-commit <sha>` / `… --squash --match-head-commit <sha>` |
| キュー待ち | `.claude/skills/merge/scripts/queue-wait.sh <PR#> [分]` |
| キューの run | `gh run list --event merge_group --limit 5` |
| キュー投入の取り消し | `gh pr merge <PR#> --disable-auto`（キュー投入済みなら PR ページの「Remove from queue」） |
