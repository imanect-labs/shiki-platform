# shiki-platform

権限考慮 RAG・自律エージェント・ミニアプリ基盤を備えるエンタープライズ AI プラットフォーム
（Rust モジュラモノリス ＋ Next.js ＋ Python ワーカー）。

設計・要件・実装順の正本は [`docs/`](./docs/) を参照:
[設計書](./docs/design.md) ・ [要件定義](./docs/requirements.md) ・ [ROADMAP](./docs/roadmap.md)。

## リポジトリ構成

```
crates/        Rust ワークスペース（api / authz / chat / agent-core / llm-gateway /
               storage / rag / sandbox-client / sandbox-orchestrator / fuse）
web/           Next.js (App Router, TypeScript) — フロント・OIDC ログイン
ingestion-worker/  Python ワーカー（後続フェーズ）
deploy/        docker compose / Keycloak realm / OTel スタック設定
scripts/       型生成などの補助スクリプト
docs/          設計・要件・ロードマップ
```

## 必要環境

- Rust（`rust-toolchain.toml` 固定版）, Docker / docker compose, Node.js + pnpm（`corepack enable pnpm`）。

## クイックスタート

`docker compose up` 一発で全依存（Postgres / Keycloak / OpenFGA / MinIO / Qdrant /
ingestion-worker）と shiki-server が起動する。

```sh
cd deploy/compose
cp .env.example .env        # 必要に応じて値を編集
docker compose up --build
```

- ingestion-worker（Docling パース・Ruri 埋め込み・reranker）は初回起動時にモデルを
  `hf-cache` volume へダウンロードする（数百 MB・以後は再利用）。
- 監視スタック（OTel / Tempo / Loki / Prometheus / Grafana）は既定で起動しない
  （開発機の RAM 節約）。使う時: `OTLP_ENDPOINT=http://otel-collector:4317 docker compose --profile observability up -d`

起動後の確認:

- ヘルスチェック: `curl http://localhost:8080/healthz` → 200
- 文書検索（RAG）: Drive にファイルをアップロード → 自動索引 → `http://localhost:3000/search`（要: 下記フロント起動）
- 監視（observability profile 起動時）: Grafana `http://localhost:3001`。

フロント（OIDC ログイン → `/me` 表示）は別途起動する:

```sh
cd web
pnpm install
cp .env.example .env.local
pnpm gen:api            # Rust 定義から型を生成（初回・API 変更時）
pnpm dev                # http://localhost:3000
```

ブラウザで `http://localhost:3000` を開き Keycloak（テストユーザー `alice` / `password`）で
ログインすると `/me` に自分の情報（org=acme, dept=engineering）が表示される。

Keycloak 管理コンソール `http://localhost:8081`、MinIO コンソール `http://localhost:9001`。

## 開発コマンド（CI の正）

```sh
# Rust
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo build

# Web
cd web
pnpm install
pnpm gen:api      # Rust 定義から OpenAPI/TS 型・認可語彙を再生成
pnpm lint
pnpm build
```

詳細な開発フロー（ブランチ → issue → 実装 → PR）は `.claude/skills/dev-workflow` を参照。
