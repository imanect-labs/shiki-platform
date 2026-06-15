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

## クイックスタート（Phase 0）

`docker compose up` 一発で全依存（Postgres / Keycloak / OpenFGA / MinIO ＋ OTel スタック）と
shiki-server が起動する。

```sh
cd deploy/compose
cp .env.example .env        # 必要に応じて値を編集
docker compose up --build
```

起動後の確認:

- ヘルスチェック: `curl http://localhost:8080/healthz` → 200
- 認証付きエンドポイント: ブラウザで `http://localhost:3000` を開き Keycloak でログイン →
  `/me` に自分の情報が表示される。
- 監視: Grafana `http://localhost:3001`（Tempo でトレース、Prometheus でメトリクス）。

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
