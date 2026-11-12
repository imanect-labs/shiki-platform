---
name: merge
description: 作業ツリーの変更を /pr で merge-ready な PR にし、CI と AI レビューが緑になったらマージキューに入れて main に入るまで見届ける。「マージまでやって」「緑になったらマージ」「/merge」の時に使う。PR を作るだけでマージしないなら /pr を使う。
---

# merge: /pr → 緑 → マージキュー → main 反映

```text
Step 1  /pr を回す            Phase 1〜6。緑（review-status.sh が exit 0）になるまで
Step 2  マージ前の確認        緑にした HEAD のまま・base が main・コンフリクトなし・draft でない
Step 3  マージキューに入れる   HEAD の SHA を固定して gh pr merge
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

- `.claude/skills/pr/scripts/review-status.sh` が **exit 0** になったら Step 2 へ。
- `/pr` の Phase 7 で**エスカレート**（3 反復で未解消）になったら、**マージせずに止めて** human に報告する。

緑にした時点の HEAD を控える（Step 3 で使う）:

```bash
GREEN_SHA=$(gh pr view --json headRefOid -q .headRefOid)
```

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
| `headRefOid` | `$GREEN_SHA` と一致 | 緑の後に push がある。Step 1 に戻る |
| `closingIssuesReferences` | 対応 Issue がある | 本文に `Closes #<n>` が無ければ足す（`dev-workflow` の規約） |

## Step 3 — マージキューに入れる

main はマージキュー必須。マージ方式はキュー側の設定で決まるので、`--squash` 等は付けない。

```bash
gh pr merge <PR#> --match-head-commit "$GREEN_SHA"
```

- `--match-head-commit` で、確認した HEAD 以外がマージされるのを防ぐ（その間に誰かが push していたら失敗する → Step 1 へ）。
- 必須チェックの一部が未完了だと auto-merge の予約になる。それで構わない（Step 4 で見届ける）。
- 「キューが無い」旨のエラーで直マージを求められたら、リポジトリ設定が変わっている。方式を推測で選ばず human に確認する。

## Step 4 — キューを見届ける

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
   - **この PR が壊した**（PR では選ばれなかったテストが落ちた）→ ブランチで直し、`/pr` の Phase 1 から回し直して Step 1 に戻る。
     テスト選択の漏れなら `ci/scenarios.toml` / `scripts/ci-select.py` の規則も同じ PR で直す。
   - **flaky・基盤障害**（この PR の変更と無関係なテスト、再実行で通る）→ 根拠を PR にコメントで残し、Step 3 から 1 回だけ再投入する。
     2 回目も落ちたら flaky と決めつけずに human に報告する。flaky なテストは Issue に切る。
   - **キュー内の前の PR とのコンフリクト** → rebase して Step 1 に戻る。
3. キュー起因のやり直しは **2 回まで**。超えたら止めて、落ちたジョブ・試したこと・必要な判断を human に報告する。

## Step 5 — 後始末と報告

1. 対応 Issue が閉じたか確かめる（`Closes #<n>` で自動クローズされる）:

   ```bash
   gh issue view <n> --json state,stateReason
   ```

   開いたままなら、マージした PR を書いた完了コメントを付けてクローズする（`--reason completed`）。

2. ローカルを最新化する（次のタスクは `origin/main` から切る）:

   ```bash
   git fetch origin main
   ```

   リモートブランチの削除はリポジトリの自動削除設定に任せる。手で消すのは human に頼まれた時だけ。

3. human に報告する:
   - PR の URL と変更の一行要約、merge commit の SHA
   - 検証内容（`/pr` の Phase 2 の結果。UI ならスクリーンショット）
   - キューで落ちてやり直したなら、その原因と対応
   - この作業中に作った Issue（番号・題名・URL）と、クローズした Issue
   - `/pr` Phase 7 のブログ価値の提案（基準を満たす時だけ）

## リファレンス

| 用途 | コマンド |
| --- | --- |
| ゲート判定（緑か） | `.claude/skills/pr/scripts/review-status.sh [PR#]` |
| マージキューに入れる | `gh pr merge <PR#> --match-head-commit <sha>` |
| キュー待ち | `.claude/skills/merge/scripts/queue-wait.sh <PR#> [分]` |
| キューの run | `gh run list --event merge_group --limit 5` |
| キュー投入の取り消し | `gh pr merge <PR#> --disable-auto`（キュー投入済みなら PR ページの「Remove from queue」） |
