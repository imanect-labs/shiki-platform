---
name: pr
description: 作業ツリーの変更を merge-ready な Pull Request にする。ローカル品質ゲート（CI と同一）→ 動作確認（compose ＋ :3000 ＋ Playwright スクショ/該当 E2E）→ パフォーマンス評価 → ドキュメント整合 → コンテキスト非共有エージェントによる独立レビュー → PR 作成 → AI レビュアー解消ループ。「PR を出す」「ship する」「レビューを通す」「レビュー指摘を直す」時に /pr で使う。
---

# PR: 作成・検証・レビュー通過

現在の変更を、CI の全ゲートと AI レビュアーを通る Pull Request にするエンドツーエンド手順。

```text
Phase 1  ローカルゲート      CI と同一のコマンドを push 前に回す
Phase 2  動作確認            compose + web(:3000) + スクショ/動画 + 該当 E2E
Phase 3  自己点検            パフォーマンス評価 ＋ ドキュメント/実装 整合
Phase 4  独立レビュー         会話文脈を持たないエージェントで多観点レビュー → 敵対検証
Phase 5  PR 作成/更新        規約準拠の本文で push
Phase 6  レビュー解消ループ    CI ＋ CodeRabbit / Codex が緑になるまで（最大 3 反復）
Phase 7  完了 / エスカレート   （＋ブログ価値の提案）
```

「緑」= `gh pr checks` が全 pass **かつ** AI レビュアーの指摘が全て対応済み。
**チェックの pass は「指摘なし」を意味しない**（Phase 6 で必ず取りこぼしを判定する）。

詳細はこのファイルの隣の `references/` にある。**各 Phase に入る前に該当ファイルを読む**:

| ファイル | 内容 |
| --- | --- |
| `references/gates.md` | CI ジョブ ↔ ローカルコマンドの対応表・カバレッジ 80% ゲート・既知の罠 |
| `references/verify.md` | 起動手順・スクリーンショット/動画・変更領域 ↔ E2E spec 対応表 |
| `references/perf.md` | パフォーマンス静的観点と、条件付き実測（EXPLAIN / bundle 差分 / bench） |
| `references/doc-consistency.md` | 正本ドキュメント ↔ 実装の整合点検（Phase 3-b） |
| `references/review.md` | 独立エージェントレビューの観点・プロンプト雛形・敵対検証（Phase 4）・bot 指摘の独立判定（Phase 6） |

ヘルパースクリプト:

- `scripts/local-gates.sh [--fast]` — 差分から必要なゲートだけ選んで回し、失敗を一覧する。**先頭でワークフローのドリフトを強制検出する。**
- `scripts/ci-snapshot.sh [--check|--update]` — `.github/workflows/*.yml` を YAML として解析・正規化し `ci-jobs.snapshot` と突き合わせる。
- `scripts/dev-up.sh` — compose 依存 ＋ shiki-server(:8080) ＋ web(:3000) を検証可能な状態で起動する。
- `scripts/review-status.sh [PR#]` — CI checks ＋ 未解消スレッド ＋ **最終コミット後に付いた bot コメント**を表示。緑 `0` / ブロック `1` / エラー `2`。

関連スキル: 全体の進め方は `dev-workflow`、不変条件の詳細は `architecture-invariants`。

## サブエージェントへの委譲

このスキルでは Agent ツールの使用が明示的に要求されている（通常セッションの「ユーザが要求しない限り Agent を呼ばない」制約の対象外）。

委譲する理由は 2 つある。**混同しない。**

- **独立性** — 自分は「何を作ろうとしたか」を知っているので、diff もスクショも bot 指摘も*意図どおりに*読んでしまう。会話文脈を持たないエージェントは、プロンプトに書いたものしか知らない。
- **コンテキスト経済** — 正本ドキュメントや 50 本超の spec を読み切る予算が本体には無い。委譲すれば読むのはサブエージェント側で、返るのは所見だけ。

| Phase | 委譲するもの | 理由 |
| --- | --- | --- |
| 1 ゲート | しない | 決定的なシェル実行。判断が入らない |
| 2 動作確認 | スクショの評価・該当 spec の探索 | 独立性 ＋ 経済 |
| 3-a perf | A 静的観点の掃き出し | 独立性 |
| 3-b docs 整合 | 正本との突き合わせ全体 | 経済（正本は数万トークン） |
| 4 独立レビュー | 4 観点 finder ＋ 敵対検証 | 独立性 |
| 5 PR 作成 | しない | 本文は自分の判断で書く |
| 6 レビュー解消 | bot 指摘の妥当性判定 | 独立性 |

**全ての委譲で共通の不変条件 — プロンプトに次を書かない:**

- 変更の意図・背景・issue の内容・自分がその設計にした理由
- 「〜は意図的です」「〜は別 PR で対応します」といった先回りの弁明
- 「特に〜を見てほしい」という誘導

経済目的の委譲（3-b）でも同じにする。意図を渡すと「その意図に沿っているか」しか見なくなり、独立性の分だけ丸ごと損をする。

**委譲しないもの**: 撮影・起動・コマンド実行そのもの（往復コストだけ増える）。委譲するのは**読む・評価する**部分に限る。

### モデルは明示的に指定する

**判断を伴う委譲には `model: "opus"` を明示する。** 省略すると親セッションのモデルを継承するため、
親が軽量モデルだと**レビューだけが黙って劣化する**。しかも出力の体裁は変わらないので、
劣化したことに気づけない（このスキルが繰り返し潰してきた「信号が無いことを良い信号と読む」形）。

対象は Phase 2 のスクショ評価・3-a・3-b・4（finder と敵対検証）・6 の bot 指摘判定。
本体より弱い目でレビューさせても、本体が見落としたものは見つからない。

例外は Phase 2 の spec 探索（`subagent_type: "Explore"`）。ファイル名と文言の機械的な照合で、
判断がほとんど入らないため継承のままでよい。

## いつ使うか

- 変更を PR にして merge-ready まで持っていきたいとき。
- 既存のレビュー指摘に対応し、チェックが通るまで push したいとき。
- ユーザ可視（UI）の変更を、見た目の裏取りと共に出したいとき。

**`/pr` と明示的に言われたかどうかで適用有無を変えない。** 成果物が PR になる作業なら、
「〜を実装して」という依頼から入った場合でもこの手順を適用する。特に **Phase 4（独立レビュー）を
依頼の文言で省かない**。実際に、機能実装の依頼から作った PR で Phase 4 が抜け、
自己レビューだけで実欠陥 4 件が残る状態になったことがある（#456）。

## 前提（仮定せず確認する）

- `gh auth status` がログイン済み。未ログインなら止めて `gh auth login` を依頼する。
- `git status` / `git diff` で、作業ツリーの変更がこの PR で意図したものか確認する。
- **`main` では作業しない。** `git rev-parse --abbrev-ref HEAD` で確認し、保護ブランチ上なら先に内容の分かるトピックブランチを切る。
- base ブランチを決める。スタック PR（前段ブランチの上に積む）なら base は親ブランチであって `main` ではない。

---

## Phase 1 — ローカル品質ゲート

**`references/gates.md` を読んでから実施する。** CI（`.github/workflows/ci.yml`）と同じものをローカルで先に潰す。レビュー往復より圧倒的に安い。

```bash
.claude/skills/pr/scripts/local-gates.sh          # 差分から必要なゲートを判定して実行
.claude/skills/pr/scripts/local-gates.sh --fast   # 重いもの（deny / build / pytest）を省く
```

**ワークフローのドリフトで落ちたら、他のゲートより先にそれを解消する。** ローカルのゲート集合が CI を
網羅していない状態なので、他が全部緑でも意味がない。順序は「`gates.md`・`local-gates.sh`・
`AGENTS.md` の検証コマンド表を差分に追随させる → `ci-snapshot.sh --update`」。
逆順にすると検出した意味が無くなる。

自分でコマンドを組む場合の必須 4 点（`gates.md` に全量と条件がある）:

1. **新規ファイルは先に `git add`** する。`check-file-size.sh` は git-tracked のみ数えるため、未追加ファイルはローカルをすり抜けて CI で落ちる。
2. **合否を見たいコマンドをパイプにつながない。** パイプ終端の exit code を返すので失敗が exit 0 に化ける（`cargo ... | tail` も `gh pr checks --watch | tail` も同じ）。`cmd > log 2>&1; rc=$?` で受ける。
3. **新規 `.rs` ファイルを足したらカバレッジ 80% ゲートを意識する。** 総計行カバレッジのため、テストの無い新規ルートファイルは総計を 80% 未満へ引きずり落とす。
4. リポジトリ規約（CLAUDE.md / `architecture-invariants`）を守る: `unwrap()`/panic 禁止、fallible な呼び出しの `let _ =` 握り潰し禁止、`?` で伝播。不変条件（単一チョークポイント / AuthContext / 二段 authz / トレイト境界 / codegen が正）を破らない。

## Phase 2 — 動作確認（UI・挙動が変わる変更のみ）

**`references/verify.md` を読んでから実施する。**

「lint と build が通ること」と「見た目・体験が良いこと」は別物。このプロジェクトはデザイン・UI/UX に妥協しない方針なので、**スクリーンショットを見ずに UI を出さない**。

```bash
.claude/skills/pr/scripts/dev-up.sh    # compose 依存 → shiki-server(:8080) → web(:3000)
```

RAG・サンドボックス・Office は既定でオフ。該当機能を触るなら `--rag` / `--sandbox` / `--compose --office` を付ける（オフのまま検証して「動かない」と誤診しない）。Office は compose 必須、RAG は native 必須（いずれもポート/到達性の制約・`verify.md` 参照）。初回の native ビルドは十数分かかる。

1. 起動して生存確認する（`curl -fsS localhost:8080/healthz`、`http://localhost:3000`）。
2. **スクリーンショットを撮って自分の目で見る** — `deviceScaleFactor: 2`、ライト/ダーク両テーマ、狭幅、パネル開閉。動きのある UI（遷移・共同編集・ドラッグ）は `video: 'on'` で録画する。
3. **撮った画像を、意図を知らないサブエージェントにも評価させる**（プロンプト雛形は `verify.md`）。自分の目だけだと「作ったとおりに見える」バイアスがかかる。画像パスと設計言語のルールだけを渡し、**何を作ったかは書かない**。
4. 崩れ・違和感を直して**再撮影する。納得いくまで反復する。**
5. **変更領域に対応する既存 E2E spec を実行して回帰を見る。** どの spec が該当するかの探索は委譲してよい（`verify.md` の導出手順をサブエージェントに渡す）:

   ```bash
   cd web && E2E_BASE_URL=http://localhost:3000 pnpm exec playwright test e2e/<対象>.spec.ts
   ```

   **実行件数を必ず確認する。** 存在しないパスを渡すと「0 tests」で終わり、成功と区別が付かない。

6. 最終スクリーンショットと E2E の結果を PR 本文の検証欄に載せる。

実装が存在しない（設計/ドキュメントのみの）変更ではこの Phase をスキップし、PR 本文に「N/A（docs のみ）」と書く。

## Phase 3 — 自己点検（パフォーマンス ＋ ドキュメント整合）

### 3-a. パフォーマンス評価

**`references/perf.md` を読んでから実施する。** CLAUDE.md のコーディング規約（「パフォーマンスを追求する」「全件取得→フィルタではなく、最初から必要なデータ・フィールドのみ取得する」）は PR ゲートとして実際に効かせる。

3 層で見る（詳細と閾値は `perf.md`）:

- **A. 静的観点**（全 PR）— 全件取得→フィルタ / N+1（ループ内クエリ・ループ内 FGA check）/ 新規 WHERE・ORDER BY 条件のインデックス有無 / 直列 await の並列化余地 / 無制限の並列度 / 不要な `clone()`・大きなコピー / SSE のバッファリング / web の不要な再レンダリング。
- **B. 相対回帰**（変更領域に応じて）— SQL を足したら `EXPLAIN (ANALYZE, BUFFERS)`、`web/` なら First Load JS の base 差分、sandbox なら `SANDBOX_BENCH=1`。
- **C. 絶対観測**（触った領域）— base と同じでも遅いものは遅い。1 リクエストの SQL 発行本数・`latency_ms`・First Load JS・体感を単体で評価する。閾値は**合否ゲートではなく調査トリガ**。

**A（静的観点）の掃き出しはサブエージェントに委譲する**（雛形は `perf.md`）。劣化パターンは「自分が書いた理由」を知っていると正当化して読み飛ばすため、独立性が効く。**B・C の実測は委譲しない**（起動・計測は自分でやる方が速い）。

A・B で見つけた劣化はこの PR で直す。C で見つけた**既存の**遅さは、範囲内なら直し、**範囲外なら issue に切って PR 本文に番号を書く**。「気づいたが放置」はしない（スコープを広げないことと、見なかったことにすることは違う）。

### 3-b. ドキュメント/実装 整合性チェック

**`references/doc-consistency.md` を読んでから実施する。この Phase は丸ごとサブエージェントに委譲する。**

正本（CLAUDE.md 記載）は合計で数万トークンあり、本体の予算では読み切れない。読み切れないまま「たぶん大丈夫」と進めると、正本が変わったことに気づかず陳腐化した記述のまま push することになる（実際に踏んだ事故は `doc-consistency.md` に記録がある）。

- 委譲するのは**突き合わせと所見の列挙**まで。**どう直すかの判断と編集は自分でやる。**
- **軽微・明白な事実差**（コマンド名・ポート・env 名・リンク・スクリプト名・しきい値）→ 同じ PR 内で直し、PR 本文に「ドキュメント整合」節として明記する。
- **設計判断・要件・優先順位・relation schema・トレイト境界**に関わる乖離 → 勝手に変更せず `AskUserQuestion` で human に確認する。どのドキュメントをどう直すか具体案を添える。
- roadmap のフェーズ完了条件を満たしたなら、roadmap / issue の更新もこの PR に含める。
- 乖離が無ければ何もしない。

## Phase 4 — 独立エージェントレビュー（コンテキスト非共有）

**`references/review.md` を読んでから実施する。** ここでは Agent ツールの使用が明示的に要求されている。

自分は「何を作ろうとしたか」を知っているので、意図通りに読んでしまい欠陥を見落とす。それを打ち消すため、**会話文脈を一切持たないサブエージェント**に diff だけを渡してレビューさせる。

1. diff をファイルに落とす（プロンプトに意図を書かないため）:

   ```bash
   # base は必ず remote-tracking ref。ローカルの main は古いことが多く、
   # `main...HEAD` は無関係な数千ファイルを差分に含めてしまう。
   BASE_REF=$(git symbolic-ref --short refs/remotes/origin/HEAD 2>/dev/null); BASE_REF=${BASE_REF:-origin/main}
   git rev-parse --verify --quiet "$BASE_REF" >/dev/null \
     || { echo "base ref が見つかりません: $BASE_REF（git fetch origin）"; exit 1; }

   # この Phase はコミット前なので、**コミット済み差分だけでは実装が丸ごと抜ける**。
   # staged / unstaged / 未追跡も含めないと、空の diff を渡して「指摘なし」で通ってしまう。
   git add -A                                   # 未追跡ファイルも diff に載せる
   { git diff "$BASE_REF"...HEAD; git diff --cached; } > "$SCRATCH/pr-diff.patch"

   # 空でないことを必ず確認する（空なら base か add が誤っている）
   [ -s "$SCRATCH/pr-diff.patch" ] || { echo "diff が空。base と作業ツリーを確認"; exit 1; }
   wc -l "$SCRATCH/pr-diff.patch"
   ```

2. **観点別の finder を並列起動する**（4 観点・1 メッセージで同時に投げる）: 不変条件/認可・パフォーマンス・ドキュメント整合・正確性/エッジケース。プロンプトには **diff のパスと読むべき正本ドキュメントのパス、観点、出力形式だけ**を書く。**変更の意図・issue の背景・自分の設計理由は渡さない**（渡した時点で独立性が失われる）。
3. **各所見を敵対検証する** — 別のサブエージェントに「この指摘を**反証**せよ。実際に失敗する入力・状態を示せないなら REFUTED」と投げる。反証されたものは捨てる。
4. 生き残った所見のみ対応する。対応・却下の判断と理由を PR 本文に 1〜2 行で残す。

修正したら Phase 1（該当ゲート）と、UI が変わったなら Phase 2 をやり直す。

## Phase 5 — PR 作成 / 更新

1. 意味のある単位でコミットして push する。メッセージは**日本語・命令形**（例 `feat(storage): フォルダ共有のReBACタプル付与を追加`）:

   ```bash
   git add -A && git commit -m "<日本語・命令形の要約>"
   git push -u origin HEAD
   ```

   push が network エラーで失敗したら指数バックオフ（2s/4s/8s/16s）で最大 4 回再試行。

2. このブランチの PR が既にあれば（`gh pr view`）新規作成せず更新する。
3. base は「前提」で決めたもの。スタックなら親ブランチを指定する。
4. 本文テンプレ:

   `--base` には**ブランチ名**を渡す。Phase 4 の `$BASE_REF`（`origin/main`）をそのまま使うと、
   その名前のブランチは GitHub 側に存在しないため PR 作成に失敗する。

   ```bash
   BASE=${BASE_REF#origin/}       # origin/main → main
   gh pr create --base "$BASE" --title "<タイトル>" --body "$(cat <<'EOF'
   <何を・なぜ変えたか>

   Closes #<n>

   ## 検証
   <スクリーンショット / 実行した E2E spec と結果 / /healthz、または N/A（docs のみ）>

   ## パフォーマンス
   <静的観点の確認結果と、実測したなら数値（EXPLAIN / First Load JS 差分 / bench）。影響なしなら「影響なし」>

   ## ドキュメント整合
   <併せて更新した docs/skill、または N/A>

   ## 独立レビュー
   <検証を生き残った所見と対応。所見なしなら「指摘なし」>
   EOF
   )"
   ```

   タイトルは内容が分かる簡潔なもの。スコープが 1 クレートに明確なら接頭にクレート名を付けてよい（例 `storage: 共有ReBACタプル付与を追加`）。

5. **base が `main` 以外（スタック PR）なら、CodeRabbit を明示トリガする** — 自動レビューが走らないため:

   ```bash
   gh pr comment <PR#> --body "@coderabbitai review"
   ```

   force-push で HEAD が変わったら再トリガする。

## Phase 6 — レビュー解消ループ（緑まで駆動）

`review-status.sh` が `0` で exit するまで繰り返す。**3 反復**で打ち切り（Phase 7）。

1. チェックが確定するまで待ち、状態を読む:

   ```bash
   # ⚠️ `gh pr checks --watch` を push 直後にそのまま実行しない。
   #    チェックが 1 件も無いと "no checks reported" を出して **exit 0 で即終了**する。
   #    push 直後は必ずこの状態なので、「緑になった」と誤読する（実際に踏んだ）。
   #    まず checks が生成されるのを待つ。
   for _ in $(seq 1 20); do
     [ "$(gh pr checks 2>/dev/null | grep -c .)" -gt 1 ] && break
     sleep 15
   done
   gh pr checks --watch --interval 30
   .claude/skills/pr/scripts/review-status.sh
   ```

   20 回待っても 1 件も出ないなら、**CI が回っていない**。`paths-ignore` 対象のみの変更か、
   **PR がコンフリクトしている**（`mergeable: CONFLICTING` だと GitHub はマージ ref を作れず
   `pull_request` の run を生成しない）。`gh pr view --json mergeable` で確かめる。

   - exit `0` → 緑。Phase 7 へ。
   - exit `1` → ブロック。出力に失敗チェック・未解消スレッド・**最終コミット後に付いた bot コメント**が並ぶ。
   - exit `2` → 取得エラー（PR 無し / 未認証）。解決して再試行。

2. **全 bot（CodeRabbit・Codex）の指摘を必ず読む。** チェックの緑と「指摘なし」は別物。

   `review-status.sh` は **未解決かつ未返信**のスレッドを、コミット時刻に関係なく列挙してブロックする。
   返信済みのものは件数だけ出す（Codex はスレッドを解決せず、CodeRabbit も
   「解決できなかったので手動で解決して」と返すことがあるため、未解決のみでブロックすると
   恒久的に緑にならない）。**「返信済みだが未解決」の件数が出ていたら、議論が実際に
   終わっているか一度は自分で見る。**

   さらに **「最終コミット後のコメントが無い」＝「対応済み」ではない**。bot がまだ最新コミットを
   見ていないだけのことがある（push 直後は必ずこの状態）。**「見た結果、指摘が無い」のか
   「まだ見ていない」のか**を必ず区別すること。

   ただし「review オブジェクトが無い＝未レビュー」でもない。CodeRabbit は指摘ゼロだと review を
   作らず commit status だけ出し、Codex は push ごとの再レビューをしないことがある。
   `review-status.sh` は **HEAD のチェックが実行中（pending）の時だけブロック**し、
   「review を残していない bot」は参考情報として出す（ブロックすると恒久的に緑にならないため）。`review-status.sh` の「最終コミット後のコメント」欄が空でも、初回は生コメントを一読する:

   ```bash
   gh api repos/{owner}/{repo}/pulls/<n>/comments --jq '.[] | "\(.created_at) [\(.user.login)] \(.path):\(.line // .original_line) \(.body[0:200])"'
   ```

3. **「却下する」と判断した指摘は、サブエージェントに独立判定させる**（雛形は `review.md`）。
   自分の実装に対する指摘の妥当性を自分で判定すると、庇う方向に倒れる。
   指摘の本文と対象ファイルだけを渡し、**自分がなぜ却下したいかは書かない**。
   採用する指摘には不要（直すのだから判定は要らない）。判定が割れたら人間に上げる。

4. 各指摘に対応する:
   - 妥当ならコードを直す。関連する Phase 1 のゲートを再実行し、clippy/test を再び壊さない。
   - 誤検知やスコープ外でも**黙って無視しない**。根拠をスレッドに返信する:

     ```bash
     gh api repos/{owner}/{repo}/pulls/<n>/comments/<comment_id>/replies -f body="..."
     ```

     コメント単体の取得は `/repos/{o}/{r}/pulls/comments/<id>`（`/pulls/<n>/comments/<id>` は **404**）。
   - 実際の修正や明確な正当化なしに、著者代理でスレッドを resolve しない（ゲートを無意味化する）。
5. UI/挙動が変わったら Phase 2 をやり直し、スクリーンショットを取り直す。
6. commit / push して（AI レビュアーが再トリガされる）ステップ 1 に戻る。

## Phase 7 — 完了 or エスカレート

- **緑:** PR リンク（`gh pr view --json url`）、変更の一行要約、検証内容（UI ならスクリーンショット）を報告する。**ユーザが明示的に依頼しない限り merge しない。**
- **3 反復後も未解消:** ループを止める。残るチェック/スレッド、試したこと、ユーザに必要な判断や権限を簡潔に報告する。投機的修正を push し続けない（reviewer と CI を浪費する）。

### ブログ価値の提案（提案のみ・自動で書かない）

緑で報告済みになったら、その変更が記事にする価値の学びを含むか判断し、**満たす時のみ提案する**:

- トレードオフのある非自明な設計判断/アーキテクチャ転換。
- 根本原因が一般化する微妙なバグ（このコードベースを超えて教訓になる）。
- ツール/ライブラリ/プラットフォーム挙動についての驚きの発見。

ルーチンな機能追加・機械的リファクタ・依存更新・docs のみ・些末な修正では提案しない。提案は 1 回まで（断られたら以後しない）。基準を満たすなら 1〜2 行のピッチを出し、ブログ issue を切るか尋ねる。承認されたら diff が新鮮なうちに issue 化する（`path:line` 参照が価値）。ラベルは領域に合わせる（例 `area:docs` / `area:web`）。エスカレート時はスキップ。

---

## リファレンス

| 用途 | コマンド |
| --- | --- |
| 現在のブランチ | `git rev-parse --abbrev-ref HEAD` |
| ローカルゲート | `.claude/skills/pr/scripts/local-gates.sh [--fast]` |
| 検証環境の起動 | `.claude/skills/pr/scripts/dev-up.sh` |
| E2E（既存 dev 再利用） | `cd web && E2E_BASE_URL=http://localhost:3000 pnpm exec playwright test e2e/<x>.spec.ts` |
| チェック監視 | `gh pr checks --watch --interval 30` |
| ゲート判定 | `.claude/skills/pr/scripts/review-status.sh [PR#]` |
| bot コメント（生） | `gh api repos/{owner}/{repo}/pulls/<n>/comments` |
| インライン返信 | `gh api repos/{owner}/{repo}/pulls/<n>/comments/<id>/replies -f body="..."` |

**ゲート扱いの AI レビュアー**: `coderabbitai[bot]`、`chatgpt-codex-connector[bot]`。bot 集合が変わる場合は `PR_REVIEW_BOTS`（空白区切り）で上書きする。
