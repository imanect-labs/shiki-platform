<!--
メンテナ向けメモ（HTML コメントはコンテキスト注入時に除去される）:
- CLAUDE.md はこのファイルへの symlink。編集はここだけ。
- 方針: コードを読めば分かること（ディレクトリ構成・依存一覧・関数名）と、
  どのリポジトリでも同じ心得（「読みやすく書く」等）は書かない。
  ここに書くのは「コードからは読み取れない前提・決まりごと・振る舞い」だけ。
- 各行に「これを消したらエージェントが誤るか？」を問い、No なら消す。100 行以内を維持する。
- 手順はスキル（.claude/skills/）、設計の根拠は docs/ に置き、ここには再掲しない。
-->

# shiki-platform エージェント指示

権限考慮 RAG・自律エージェント・ミニアプリ基盤を備えるエンタープライズ AI プラットフォーム
（Rust モジュラモノリス ＋ Next.js ＋ Python ワーカー）。

## タスク開始時

**main は古いことがある。着手前に必ず最新化し、そこからブランチを切る。**

```bash
git fetch origin main
git switch -c <branch-name> origin/main   # 作業中のブランチは git rebase origin/main
```

- main で直接作業しない。1 タスク = 1 ブランチ = 1 GitHub Issue（手順は `dev-workflow` スキル）。
- 実装前に `docs/design-caveats.md` に該当する落とし穴（PIT-*）が無いか確認する。
- そのタスクがどのフェーズ・どの依存に属するかを `docs/roadmap.md` で確認する。

## 振る舞い

- **検証してから完了と言う。** 変更したレイヤの検証コマンド（後述）を実際に流し、出力を示す。通していないものを「完了」と報告しない。
- **推測で埋めない。** 仕様が曖昧なら正本ドキュメントを読む。それでも決まらなければ human に聞く。
- **human に必ず確認する:** OpenFGA relation schema（ポリシ決定）／トレイト境界の変更／タスクの優先順位／外部に出る操作（PR 作成・push 先の変更・デプロイ）／破壊的操作。
- **UI 変更は実際に起動して目で確かめる**（`pr` スキルの手順）。「動くはず」で出さない。余白・状態遷移・ローディング・エラー表示・キーボード操作まで詰め、妥協した箇所は PR に明記する。
- **ドキュメントと実装の乖離は勝手に直さない。** 正本は docs/ 側なので、human に修正を提案する。
- **範囲を勝手に広げない。** ただし着手中のタスクの範囲内で見つけた前フェーズの不備は直す。
- コミット・PR・応答は日本語。コミットメッセージは命令形で簡潔に（例: `feat(storage): フォルダ共有の ReBAC タプル付与を追加`）。

## 必ず守る不変条件

破ると認可バイパス・テナント越境に直結する核。詳細チェックリストは `architecture-invariants` スキル、根拠は `docs/design.md` §1,§4,§5。

- **単一チョークポイント:** ストレージ = StorageService／認可 = OpenFGA クライアント／LLM = llm-gateway を必ず経由する。個別ハンドラに権限チェックを散らさない。
- **アンビエント権限の禁止:** 全データアクセスは `AuthContext { principal, org, tenant_id }` 経由。tenant_id が落ちる経路を作らない。
- **二段 authz:** RAG・構造化データは pre-filter ＋ post-filter の両方を通す。実効権限 = スコープ ∩ ユーザー ReBAC。片方が壊れても権限が守られること。
- **差し替えはトレイト裏で:** cloud/onprem 差は ObjectStore / VectorStore / LlmProvider / Sandbox / DocumentParser / EmbeddingProvider で吸収し、アプリ本体を分岐させない。
- **codegen が正:** 型（Rust → OpenAPI → TS、SSE は ts-rs/typeshare）と認可語彙（relation・スコープ・ツール名）は単一定義から生成する。手書きの対応物を作らない。

## コーディング規約（既定と異なる点のみ）

- 1 ファイル 1000 行以内（`*.rs`・CI ゲート）。超えたら責務で分割する。
- 全件取得 → フィルタではなく、最初から必要な行・フィールドのみ取得する。
- 新規 migration は既存の最大番号 +1。番号重複は新規 DB でだけ壊れるため CI で弾いている。
- `vendor/` は所有フォーク。品質ゲート（1000 行 / カバレッジ / clippy / machete）の対象外で、`docs/sandbox/fork-policy.md` に従う。サンドボックス由来の入力は敵対的として扱う。
- workspace 外のクレート（`vendor/secure-exec`・`crates/script-runtime/{guest,fuzz}`・`crates/tabular/runner`）はワークスペースのコマンドでは検査されない。`--manifest-path` で個別に回す。

## 検証コマンド（`.github/workflows/ci.yml` が正）

変更したレイヤの分だけ流せばよい。

| 対象 | コマンド |
| --- | --- |
| Rust | `cargo fmt --all --check` ／ `cargo clippy --all-targets --all-features -- -D warnings` ／ `cargo nextest run --workspace --all-features`（絞るなら `-p <crate>`） |
| 品質ゲート | `bash scripts/check-file-size.sh` ／ `bash scripts/check-migration-versions.sh` ／ `cargo machete crates` ／ `cargo deny --all-features check` |
| カバレッジ | `cargo llvm-cov --all`（CI は行カバレッジ 80% 未満で fail。除外パターンは ci.yml 参照） |
| Web | `cd web && pnpm install --frozen-lockfile && pnpm gen:api && pnpm lint && pnpm build`（E2E は `pnpm e2e`） |
| Python | `cd ingestion-worker && uv sync --frozen && uv run ruff check . && uv run pytest -m "not slow" -q` |
| 統合 | `cd deploy/compose && cp .env.example .env && docker compose up -d --build shiki-server && bash smoke-bff.sh` |

結合テストは環境変数が無いと**黙ってスキップ**される（`OPENFGA_TEST_URL`・`STORAGE_TEST_DATABASE_URL`・`RAG_TEST_QDRANT_URL` 等）。「緑だった」ではなく、意図したテストが実際に走ったかを確認する。

## 正本ドキュメント

アーキテクチャの詳細はここにあり、本ファイルには再掲しない。

| 知りたいこと | 場所 |
| --- | --- |
| 設計原則・全体構成・サブシステム・リポジトリ構成 | `docs/design.md` |
| 機能要件（FR-1〜17）・非機能要件 | `docs/requirements.md` |
| ミニアプリ／ワークフロー／shiki script／skill／シークレット | `docs/miniapp-platform.md` |
| 実装順・フェーズ・依存関係 | `docs/roadmap.md` ＋ `docs/roadmap/phase-*.md` |
| 実装前に潰すべき落とし穴（PIT-*） | `docs/design-caveats.md` |
| 用語・セキュリティモデル入門 | `docs/guides/mini-app-onboarding.md` |

## スキル（`.claude/skills/`）

- `dev-workflow` — ブランチ → Issue → 実装 → PR → close の進め方
- `architecture-invariants` — 不変条件の詳細チェックリスト
- `pr` — PR 作成から CI・AI レビュー通過までのループ

## Cursor Cloud specific instructions

Cloud Agent VM 特有の非自明な起動時の注意（依存導入は update script が済ませる。ここには書かない）。

- **Docker はデーモンを手動起動する。** インストール済みだが自動起動しない。各セッション開始時に一度だけ:
  `sudo dockerd > /tmp/dockerd.log 2>&1 &` の後 `sudo chmod 666 /var/run/docker.sock`。依存
  （Postgres/Keycloak/OpenFGA/Redis/MinIO 等）は全て docker compose 前提なので、これが無いと何も起動できない。
- **フルスタック起動は `.claude/skills/pr/scripts/dev-up.sh`（native 既定）を使う。** compose で依存だけ起動し、
  shiki-server を `cargo run`（:8080・cold build は数分）・web を `pnpm dev`（:3000）で立てる。RAG/sandbox が要る時だけ
  `--rag`/`--sandbox`（`--rag` は ingestion-worker の初回モデル DL で重い）。ログイン導線は
  `http://localhost:3000` → Keycloak（`alice` / `password`）→ 認証済みホーム。ユーザ情報は `/api/me`（`/me` ルートは無い）。
- **pnpm は 10 系（corepack で 10.33.3 を activate 済み）。** pnpm 11 は `pnpm install` 時に gitignore されない
  `web/pnpm-workspace.yaml`（allowBuilds スタブ）を作り作業ツリーを汚す。plain `pnpm` が 11 に戻ったら
  `COREPACK_ENABLE_DOWNLOAD_PROMPT=0 corepack prepare pnpm@10.33.3 --activate` で戻す。
- **`pnpm build` は web の dev サーバを止めてから流す。** `next dev`（dev-up の web）と `pnpm build` は `web/.next` を
  共有するため、同時に走らせると `Cannot find module for page` でビルドが壊れる（lint `pnpm lint` は同時可）。
- **`pnpm gen:api`（＝ `pnpm build` の prebuild）は `crates/*/bindings/` の ts-rs 生成物（追跡対象）を書き換えることがある。**
  意図しない差分が出たら `git checkout -- crates/gui/bindings crates/workflow-engine/bindings` で戻す。
- `uv` は `~/.local/bin/uv`（ログインシェルは PATH 済み）。`cargo nextest`・`cargo machete` も導入済み。
