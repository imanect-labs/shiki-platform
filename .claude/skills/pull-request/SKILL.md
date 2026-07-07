---
name: pull-request
description: 作業ツリーの変更を merge-ready な Pull Request にし、CI チェックと AI レビュアー（CodeRabbit / Codex）が全て緑になるまで回す。ローカル品質ゲート実行 → PR 作成/更新（このリポジトリの PR 規約準拠）→ レビュー解消ループ。UI 変更は 0.0.0.0:{10000+issue番号} で起動して動作検証する。「PR を出す」「ship する」「レビューを通す」「レビュー指摘を直す」時に /pull-request で使う。
---

# Pull request: 作成・検証・レビュー通過

現在の変更を、このリポジトリの自動品質ゲートを通る Pull Request にし、レビュー指摘を緑になるまでループで直すエンドツーエンド手順。緑になったらブログ価値のある変更を提案する（Phase 6）。

このリポジトリの品質ゲートは（GitHub Actions がまだ無いため）次の2つ:

1. **ローカルゲート**（push 前に自分で回す）: `cargo fmt` / `cargo clippy` / `cargo build` / `cargo test`、web 差分があれば `pnpm lint` / `pnpm build`。
2. **AI レビュアー**（PR のステータスチェック＆レビュースレッド）:
   - `CodeRabbit`（`coderabbitai[bot]`）
   - `Codex`（`chatgpt-codex-connector[bot]`）

「緑」= `gh pr checks` が全て pass **かつ** 上記 bot の未解消レビュースレッドが無い状態。以下のループでそこへ駆動する。

関連スキル: 全体の進め方は `dev-workflow`、アーキ/セキュリティ不変条件は `architecture-invariants`。

## いつ使うか

- 変更を PR にして merge-ready まで持っていきたいとき。
- 既存のレビュー指摘に対応し、チェックが通るまで push したいとき。
- ユーザ可視（UI）の変更で、動作の裏取りと共に出したいとき。

## 前提（仮定せず確認する）

- `gh auth status` がログイン済み。未ログインなら止めて `gh auth login` を依頼する。
- 作業ツリーの変更がこの PR で意図したものか（`git status` / `git diff`）。
- 動作検証する場合: `scripts/launch-app.sh` が `web/`（Next.js）/ `crates/api`（axum）を起動できること。実装が未着手なら検証はスキップ可（後述）。

ヘルパースクリプトはこのファイルの隣にある:

- `scripts/review-status.sh [PR#]` — CI checks ＋ 未解消 AI レビュースレッドを表示。緑 `0` / ブロック `1` / エラー `2` で exit。
- `scripts/launch-app.sh <issue番号> [web|api|both]` — `0.0.0.0:{10000+issue番号}` でアプリを起動し、起動 URL を表示する。

---

## Phase 1 — ブランチとローカル品質ゲート

1. **`main`（や検出した統合ブランチ）では作業しない。** `git rev-parse --abbrev-ref HEAD` で確認。保護ブランチ上なら先にトピックブランチを切る（内容が分かる名前。例 `git switch -c fix-project-panel-crash`）。
2. ローカルゲートを回し、報告された問題を **push 前に** 全て直す（レビュー往復より安い）。CLAUDE.md「コマンド（CI の正）」が正:

   ```bash
   cargo fmt --all
   cargo clippy --all-targets -- -D warnings
   cargo build
   cargo test
   ```

   web 差分（`web/` を含む）があれば:

   ```bash
   pnpm install
   pnpm gen:api      # utoipa → openapi-typescript（手書き型を作らない）
   pnpm lint
   pnpm build
   ```

3. リポジトリ規約（CLAUDE.md / `architecture-invariants`）を守る: `unwrap()`/panic 禁止、fallible 呼び出しの `let _ =` 握り潰し禁止、`?` でエラー伝播。不変条件（単一チョークポイント / AuthContext / 二段authz / トレイト境界 / codegen が正）を破らない。AI レビュアーもここを突くので、今直しておくとループが減る。

## Phase 2 — 動作検証（UI/挙動が変わる変更のみ）

レイアウト・パネル・ナビゲーション・API 応答など、レンダリング/挙動が変わるものは実際に起動して裏を取る。スクリーンショットは不要。代わりに **`0.0.0.0:{10000+issue番号}` で起動**して確認する。

```bash
# issue 番号から PORT=10000+issue を計算し、変更箇所に応じて web/api/both を自動起動する。
.claude/skills/pull-request/scripts/launch-app.sh <issue番号>            # 自動判定
.claude/skills/pull-request/scripts/launch-app.sh <issue番号> web       # 明示指定も可
```

- target を省略すると `git diff --name-only <base>...HEAD` から判定する（`web/` → web、`crates/`・`ingestion-worker/` → api、両方 → both）。
- 起動後、生存確認する: backend は `curl -fsS http://0.0.0.0:$PORT/healthz`、web はトップページ。URL（`http://0.0.0.0:{port}`）をユーザに提示する。
- **実装が未着手**（`web/` も `crates/api` も無い設計フェーズ）なら、スクリプトはスキップを表示して正常終了する。その旨を検証欄に書く。

## Phase 2.5 — ドキュメント/実装 整合性チェック

PR を緑にする過程で、**プロジェクトの使い方・要件の変更**や、**正本ドキュメントと実装の乖離**を検知したら、このタイミングで整合性を保つよう修正を提案する。Phase 1 合格後・PR 本文確定前に実施する。

突き合わせる正本（CLAUDE.md より）:

- `docs/design.md`（設計原則・構成・トレイト境界）
- `docs/requirements.md`（FR-1〜11・非機能要件）
- `docs/roadmap.md` ＋ `docs/roadmap/phase-*.md`（実装順・依存・完了条件）
- `CLAUDE.md`(AGENTS.md)（コマンド・不変条件）
- `.claude/skills/*`（`dev-workflow` / `architecture-invariants` / 本スキル自身）

検知観点の例:

- コマンド／ポート／env 名／バイナリ名・スクリプト名がドキュメント記載と食い違う（例: `pnpm gen:api`、`AXUM_BIND_ADDR`、`/healthz`・`/me`）。
- 新規/変更した API・relation・スコープ・ツール名が requirements/design の語彙や codegen 単一定義と不整合。
- roadmap のフェーズ/依存と実装範囲がずれる、または完了条件を満たした（issue/roadmap 更新が必要）。
- 不変条件（単一チョークポイント・AuthContext・二段authz・トレイト境界・codegen が正）への逸脱、または記述更新が必要な変化。

取り扱い:

- **軽微・明白な事実差**（コマンド名/ポート/リンク等）→ 同じ PR 内でドキュメント/skill も併せて更新し、PR 本文に「ドキュメント整合」節として明記する。
- **設計判断・要件・優先順位・relation schema・トレイト境界**に関わる乖離 → 勝手に変更せず、`AskUserQuestion` で human に確認する（CLAUDE.md「判断に迷ったら human に相談」「公開操作は確認」準拠）。どのドキュメントをどう直すか具体案を提示する。
- 乖離が無ければ何もしない。

## Phase 3 — PR 作成 / 更新

1. 明確なメッセージでコミットして push する:

   ```bash
   git add -A && git commit -m "<日本語・命令形の要約>"
   git push -u origin HEAD
   ```

   コミットメッセージは **日本語・命令形・意味のある単位**（`dev-workflow` 準拠。例 `feat(storage): フォルダ共有のReBACタプル付与を追加`）。push が network エラーで失敗したら指数バックオフ（2s/4s/8s/16s）で最大4回再試行。

2. このブランチの PR が既にあれば（`gh pr view`）、新規作成せず更新する。
3. base ブランチは統合ブランチを検出して使う（`git symbolic-ref --short refs/remotes/origin/HEAD` 由来、既定 `main`）。
4. PR 本文には **目的 / 対応 Issue（`Closes #<n>`）/ 検証方法**（起動 URL・`/healthz` 結果、または「N/A」）を含める（`dev-workflow` 完了時要件）。

   ```bash
   BASE=$(git symbolic-ref --short refs/remotes/origin/HEAD 2>/dev/null | sed 's@^origin/@@'); BASE=${BASE:-main}
   gh pr create --base "$BASE" --title "<タイトル>" --body "$(cat <<'EOF'
   <何を・なぜ変えたか>

   Closes #<n>

   ## 検証
   <起動 URL・/healthz の結果と確認内容、または N/A>

   ## ドキュメント整合
   <併せて更新した docs/skill、または N/A>
   EOF
   )"
   ```

   PR タイトルは内容が分かる簡潔なもの。1クレートが明確なスコープなら接頭にクレート名を付けてよい（例 `storage: 共有ReBACタプル付与を追加`）。

## Phase 4 — レビュー解消ループ（緑まで駆動）

`review-status.sh` が `0` で exit するまで繰り返す。**3反復**で打ち切り（Phase 5 参照）。

1. **ゲートが確定するまで待ち**、読む:

   ```bash
   gh pr checks --watch --interval 30      # チェックが終わる（or 失敗）までブロック
   .claude/skills/pull-request/scripts/review-status.sh
   ```

   - exit `0` → 緑。「完了」へ。
   - exit `1` → ブロック。出力に失敗チェックと未解消 AI スレッド（`[bot] path:line: コメント`）が並ぶ。続行。
   - exit `2` → 取得エラー（PR 無し / 認証）。解決して再試行。

2. **未解消スレッドを各々その内容に基づいて対応する**:
   - 指摘が正しければコードを直す。関連するローカルゲート（Phase 1）を再実行し clippy/test を再び壊さない。
   - 誤検知やスコープ外なら、黙って無視せず根拠をスレッドに返信する: `gh pr comment <PR#> --body "..."`（必要なら API でインライン返信）。
   - 実際の修正や明確な正当化なしに、著者代理でスレッドを resolve しない（ゲートを無意味化する）。
3. UI/挙動が変わったら **Phase 2 を再実行**し、起動確認を取り直す。
4. 修正を commit/push（AI レビュアーが再トリガされる）:

   ```bash
   git add -A && git commit -m "レビュー指摘に対応" && git push
   ```
5. ステップ 1 へ戻る。

## Phase 5 — 完了 or エスカレート

- **緑:** PR が通った旨をユーザに伝える — リンク（`gh pr view --web` の URL）、変更の一行要約、検証内容（UI なら起動 URL）。ユーザが明示的に依頼しない限り merge しない。
- **3反復後も未解消:** ループを止める。残るチェック/スレッド、試したこと、ユーザに必要な判断/権限を簡潔に報告する。投機的修正を push し続けない（reviewer/CI を浪費する）。

## Phase 6 — ブログ価値？（提案のみ・自動で書かない）

PR が緑で報告済み（Phase 5「緑」）になったら、その変更が記事にする価値のある学びを含むか判断し、**提案する**。記事は書かず、ユーザの go なしに何も作らない。エスカレート時はスキップ。

提案する基準（満たす時のみ）:

- トレードオフのある非自明な**設計判断/アーキテクチャ転換**。
- 根本原因が一般化する**微妙なバグ**（このコードベースを超えて教訓になる）。
- ツール/ライブラリ/プラットフォーム挙動についての**驚きの発見**。

ルーチンな機能追加・機械的リファクタ・依存更新・docs のみ・些末な修正では提案しない。提案は1回まで（断られたら以後しない）。

基準を満たすなら 1〜2 行のピッチ（角度/主張）を出し、ブログ issue を切るか尋ねる。承認されたら diff が新鮮なうちに issue 化する（`path:line` 参照が価値）。ラベルは本リポジトリの領域に合わせる（例 `area:docs` / `area:web`）。節見出しは日本語。

---

## リファレンス

| 用途 | コマンド |
| --- | --- |
| 現在のブランチ | `git rev-parse --abbrev-ref HEAD` |
| ローカルゲート | `cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo build && cargo test`（web は `pnpm lint && pnpm build`） |
| アプリ起動（検証） | `.claude/skills/pull-request/scripts/launch-app.sh <issue番号> [web\|api\|both]` |
| チェック監視 | `gh pr checks --watch --interval 30` |
| ゲート判定 | `.claude/skills/pull-request/scripts/review-status.sh [PR#]` |
| PR レビュースレッド（生） | `gh api repos/{owner}/{repo}/pulls/{n}/comments` |

**ゲート扱いの AI レビュアー bot**: `coderabbitai[bot]`、`chatgpt-codex-connector[bot]`。bot 集合が変わる場合は `PR_REVIEW_BOTS` 環境変数（空白区切り）で上書きする。**注意: これらの bot はインラインのレビューコメント（提案）を「解決済みスレッド」として扱わないことがある。マージ前に `gh api repos/{owner}/{repo}/pulls/{n}/comments` で各 bot の実コメントを必ず一読すること（チェックの pass＝指摘無しではない）。**
