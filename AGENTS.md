# AGENTS.md

あなたはシニアエンジニアとしての技術的判断基準を持つものとする。判断に迷ったら必ず human に相談すること。
また、デザイン及び UI/UX には一切の妥協を許さない。 「そんな細かいところも気にするのか」というくらいのユーザー視点を持ち、圧倒的な使いやすさと、「使っていて楽しい」「使いたくなる」というコンセプトを追求する。
## プロジェクト

shiki-platform = 権限考慮RAG・自律エージェント・ミニアプリ基盤を備えるエンタープライズAIプラットフォーム（Rust モジュラモノリス ＋ Next.js ＋ Python ワーカー）。

正本ドキュメント（必ずここを読む。アーキの詳細はここに再記述しない）:

- 設計原則・全体構成・サブシステム・リポジトリ構成・fable 5 委譲境界: docs/design.md
- 機能要件(FR-1〜11)・非機能要件: docs/requirements.md
- 実装順・フェーズ・依存関係: docs/roadmap.md ＋ docs/roadmap/phase-*.md
- 用語・セキュリティモデル入門: docs/guides/mini-app-onboarding.md

## 技術スタック

- 言語/基盤: Rust(axum / cargo workspace)・Next.js + TypeScript(pnpm)・Python(ingestion-worker / Docling)
- ステートフル依存: Postgres・Qdrant・Tantivy(+Lindera)・OpenFGA(ReBAC)・Keycloak(OIDC)・MinIO/GCS
- 隔離/推論/監視: Firecracker/gVisor・vLLM/外部API・OTel(Tempo/Loki/Prometheus)・Langfuse
- リポジトリ構成（モノレポ）は docs/design.md §5 を参照。

## コマンド（CI の正）

- Rust: cargo fmt --check / cargo clippy -- -D warnings / cargo test（単体は cargo test <name>） / cargo build
- Web: pnpm install / pnpm dev / pnpm build / pnpm lint / 型生成 pnpm gen:api（utoipa → openapi-typescript）
- 全体: docker compose up（smoke: /healthz・/me）

## 必ず守る不変条件（要点のみ）

違反しやすく代償が大きい核。詳細チェックリストは architecture-invariants スキル、根拠は docs/design.md §1,§4,§5,§6。

- 単一チョークポイント: ストレージ=StorageService / 認可=OpenFGA クライアント / LLM=llm-gateway を必ず経由。個別ハンドラに権限チェックを散らさない。
- アンビエント権限の禁止: 全データアクセスは AuthContext { principal, org } 経由。将来の tenant_id の継ぎ目を壊さない。
- 二段 authz: RAG/構造化データは pre-filter ＋ post-filter の両方。実効権限 = スコープ ∩ ユーザー ReBAC。
- 差し替えはトレイト裏で: cloud/onprem 差は ObjectStore/VectorStore/LlmProvider/Sandbox/DocumentParser/EmbeddingProvider のみで吸収。アプリ本体を分岐させない。
- codegen が正（手書き型を作らない）: 型(Rust→OpenAPI→TS、SSE は ts-rs/typeshare)・認可語彙(relation/スコープ/ツール名)は単一定義から生成。

## コーディング規約

- 全件取得→フィルタではなく、最初から必要なデータ・フィールドのみ取得する。
- パフォーマンスを追求する。
- 可読性を高める適切な変数名・関数名を使う。
- セキュリティのベストプラクティスを遵守する。認可・監査・サンドボックス・公開API境界は特に慎重に（confused-deputy 防御・authz バイパス禁止）。

## 開発フロー

タスクごとにブランチを切り、issue 化 → 実装 → PR 作成 → issue クローズで進める。詳細手順は dev-workflow スキル。
コミット・PR・応答は日本語（既存リポジトリに合わせる）。
