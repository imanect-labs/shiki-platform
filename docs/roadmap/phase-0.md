# Phase 0 — 歩く骨格（Walking Skeleton）

> 目的: トレイト境界・認証/認可・配布形態（compose）・型契約・可観測性の土台を**最初に1本通す**。
> 機能価値はまだ無いが、以降の全フェーズがこの骨格に乗る。
> 完了の定義(DoD): `docker compose up` 一発で全依存が起動し、Keycloakでログインしたユーザーが
> OpenFGAで認可される `GET /me` をブラウザから叩けて、その1リクエストがOTelトレースに現れる。

## タスク一覧

| ID | タイトル | area | 依存 |
|----|---------|------|------|
| 0.1 | モノレポ/Rustワークスペース初期化 | infra | – |
| 0.2 | docker compose 基盤（依存ミドルウェア） | infra | 0.1 |
| 0.3 | axum サーバ雛形＋設定ローダ＋ヘルスチェック | infra | 0.1 |
| 0.4 | Keycloak realm 構成＋OIDC JWT 検証ミドルウェア | auth | 0.2, 0.3 |
| 0.5 | OpenFGA 配線＋authzクライアント＋認可コンテキスト | auth | 0.2, 0.3 |
| 0.6 | 認証＋認可付きサンプルエンドポイント `GET /me` をE2E | api | 0.4, 0.5 |
| 0.7 | Next.js 雛形＋OIDCログイン＋型生成パイプライン | frontend | 0.3, 0.4 |
| 0.8 | OTel 計装の土台（trace/log/metric エクスポート） | obs | 0.3 |
| 0.9 | CI（cargo check/test/clippy・web build・compose smoke） | infra | 0.1 |
| 0.10 | skillex 連携を見据えた realm/トークン設計＋発行確認 | auth | 0.4 |

---

## 詳細

### Task 0.1: モノレポ/Rustワークスペース初期化
- **area**: infra
- **依存**: なし
- **path**: リポジトリ全体
- **仕様**:
  - ルートに Cargo workspace を作成。`crates/` 配下に空のクレート骨格を切る:
    `api`, `chat`, `agent-core`, `llm-gateway`, `storage`, `rag`, `authz`, `sandbox-client`,
    `sandbox-orchestrator`, `fuse`。各クレートは `lib.rs` のみ（実装は後続フェーズ）。
  - `web/`（Next.js, App Router, TypeScript）、`ingestion-worker/`（Python・空）、`deploy/`、`docs/` を用意。
  - 共通の `rust-toolchain.toml`（固定版）、`.editorconfig`、`rustfmt.toml`、`clippy` 設定、`.gitignore`。
  - ルート `README.md` に開発の起動手順（compose）を記載。
- **受け入れ条件**:
  - [ ] `cargo build` がワークスペース全体で通る
  - [ ] `crates/` の各クレートがワークスペースメンバとして認識される
  - [ ] `web/` で `pnpm dev`（or npm）が起動する

### Task 0.2: docker compose 基盤（依存ミドルウェア）
- **area**: infra
- **依存**: 0.1
- **path**: `deploy/compose/`
- **仕様**:
  - `docker-compose.yml` に Phase 0 で必要な依存を定義: **Postgres**, **Keycloak**, **OpenFGA**, **MinIO**。
    （Qdrant/Tantivy/ingestion等は後続フェーズで追記）
  - 各サービスにヘルスチェック、永続ボリューム、`.env.example`（接続情報）。
  - OpenFGA は Postgres をバックエンドに設定。Keycloak も Postgres を使用。
  - shiki-server もサービスとして追加（後で 0.3 のイメージを参照）。
- **受け入れ条件**:
  - [ ] `docker compose up` で全サービスが healthy になる
  - [ ] MinIO/Keycloak/OpenFGA の管理UI/エンドポイントに到達できる
  - [ ] 再起動してもデータが永続する

### Task 0.3: axum サーバ雛形＋設定ローダ＋ヘルスチェック
- **area**: infra
- **依存**: 0.1
- **path**: `crates/api`
- **仕様**:
  - axum でHTTPサーバを起動。`GET /healthz`（liveness）, `GET /readyz`（依存接続確認）。
  - **設定ローダ**: 環境変数/設定ファイルから読み、`AppConfig` に集約。
    クラウド/オンプレ差し替えの起点として、各トレイト実装の選択を設定で切替える前提の構造にする
    （例: `storage.backend = "minio" | "gcs"`）。Phase 0 では値の読み込みと検証のみ。
  - Postgres 接続プール（sqlx等）を初期化し `/readyz` で疎通確認。
  - 構造化ログ（tracing）の初期化（OTelは0.8で接続）。
- **受け入れ条件**:
  - [ ] `/healthz` が200を返す
  - [ ] `/readyz` がPostgres断時に503、復帰で200
  - [ ] 設定の必須欠落時に起動エラーで明確に落ちる

### Task 0.4: Keycloak realm 構成＋OIDC 検証ミドルウェア（土台）
- **area**: auth
- **依存**: 0.2, 0.3
- **path**: `crates/api`（middleware）, `deploy/keycloak/`
> ⚠️ **認証方式は BFF + オパークセッション Cookie に確定**（ADR `docs/auth/browser-token-strategy.md` / Task 0.11 #55）。本タスクは Keycloak realm と**クレーム抽出→`principal`** までを土台として作り、`Authorization: Bearer` 検証は **Task 0.11 でセッション Cookie 検証へ置換**される（Bearer 版を最終形にしない・ブラウザにトークンを持たせる前提を残さない）。
- **仕様**:
  - Keycloak に `shiki` realm を定義（realm export JSON を `deploy/keycloak/` に commit、起動時インポート）。
    フロント用 client（BFF の Authorization Code + PKCE 用・confidential）、API用設定、テストユーザーを含む。
  - **クレーム抽出（再利用される中核）**: 検証済み OIDC クレームから `principal`（user id, email, groups/dept）を抽出し request extension に載せる（`claims.rs`）。この層は 0.11 でも再利用する。
  - **トークン検証ロジック**（JWKS取得・キャッシュ・署名/exp/aud/iss検証）は、0.11 では BFF の token 交換後の ID/Access token 検証として再利用する。`Authorization: Bearer` 入口は 0.11 で撤去。
  - SSE は Cookie 自動添付を前提にする（ヘッダ認証は不要。POST ストリームは Task 3.5 の方式に従う）。
  - 顧客IdPフェデレーション（AD/Entra/Okta）は**設定で追加できる構造**にするが、Phase 0 では shiki realm のみ。
- **受け入れ条件**:
  - [ ] 有効なトークンで保護エンドポイントにアクセスでき、無効/期限切れは401
  - [ ] JWKSのローテーションに追従（キャッシュTTL／kid不一致で再取得）
  - [ ] principal にユーザーIDと所属group/deptが入る

### Task 0.5: OpenFGA 配線＋authzクライアント＋認可コンテキスト
- **area**: auth
- **依存**: 0.2, 0.3
- **path**: `crates/authz`
- **仕様**:
  - OpenFGA クライアントクレートを実装（store作成、authorization model のロード/バージョン管理）。
  - **最小 relation model** を定義（Phase 0 は骨格のみ）: `organization`, `department`, `user`,
    relations `member`, `parent`。後続フェーズで `folder`/`file`/`thread`/`doc_chunk` を追加する前提のスキーマ構成。
  - **認可コンテキスト** `AuthContext { principal, org }` を定義し、全データアクセスがこれを受け取る規約を導入
    （将来の `tenant_id` 追加の継ぎ目）。`check(user, relation, object)` ヘルパを提供。
  - relation model は `docs/design.md` の ReBAC 図に準拠。**model定義は人がレビュー**（ポリシ決定）。
- **受け入れ条件**:
  - [ ] authorization model が OpenFGA にロードされバージョンが記録される
  - [ ] `check()` が tuple に基づき allow/deny を返す（ユニットテスト）
  - [ ] AuthContext を経由しないデータアクセスがコンパイル/レビューで弾ける設計になっている

### Task 0.6: 認証＋認可付きサンプルエンドポイント `GET /me` をE2E
- **area**: api
- **依存**: 0.4, 0.5
- **path**: `crates/api`
- **仕様**:
  - `GET /me`: JWT検証→principal取得→OpenFGAで簡単な check（例: 自分のorgのmemberか）→
    ユーザー情報（id, email, dept, org）をJSONで返す。
  - 認証ミドルウェア・authzクライアント・設定・DBが1リクエストで協調する**縦の最小貫通**を確立。
  - OpenAPI（utoipa）注釈を付け、0.7の型生成対象にする。
- **受け入れ条件**:
  - [ ] ログイン済みフロントから `/me` が自分の情報を返す
  - [ ] 別orgリソースへのcheckがdenyされる負例テスト
  - [ ] このリクエストが0.8のトレースに1本のspanツリーとして現れる

### Task 0.7: Next.js 雛形＋OIDCログイン＋型生成パイプライン
- **area**: frontend
- **依存**: 0.3, 0.4
- **path**: `web/`
> ⚠️ **ブラウザにトークンを保持しない**（BFF + オパークセッション Cookie に確定・ADR / Task 0.11 #55）。OIDC の code 受け／token 交換／refresh は**サーバ側（BFF）**が担い、フロントは `localStorage` 保持・silent renew・`Authorization` ヘッダ自動付与を**実装しない**。
- **仕様**:
  - Next.js App Router 雛形、OIDCログイン（Keycloak へリダイレクト → **BFF が code/token 交換**。ブラウザはトークンを保持せず**セッション Cookie のみ**）。
  - ログイン後に `/me` を呼んで表示する最小画面。
  - **型生成パイプライン**: `utoipa` が出すOpenAPI仕様 → `openapi-typescript` でTSクライアント/型生成。
    SSEイベント型は ts-rs/typeshare でRustから生成。生成物は commit せず CI/スクリプトで再生成可能に。
  - 認証付き fetch ラッパ（**`credentials:'include'` でセッション Cookie 送出**。SSE は Cookie 自動添付。POST ストリームは Task 3.5 方式）。
- **受け入れ条件**:
  - [ ] ブラウザでログイン→`/me`の自分情報が表示される
  - [ ] `pnpm gen:api`（等）でRust定義から型/クライアントが再生成される
  - [ ] 手書きのAPI型が存在しない

### Task 0.8: OTel 計装の土台
- **area**: obs
- **依存**: 0.3
- **path**: `crates/api`（telemetry init）, `deploy/compose/`
- **仕様**:
  - `tracing` + `opentelemetry` を初期化し、OTLPでエクスポート。compose に **Grafana/Tempo/Loki/Prometheus**
    を最小構成で追加（オンプレ既定）。クラウドはエクスポータ差し替え前提の抽象に。
  - axum リクエストに span を張り、trace_id を伝播（後で Langfuse と相関させる種）。
  - 基本メトリクス（リクエスト数/レイテンシ）とログのtrace_id付与。
- **受け入れ条件**:
  - [ ] `/me` 1回が Tempo でトレースとして閲覧できる
  - [ ] ログに trace_id が含まれ、トレースと突合できる
  - [ ] Prometheus にリクエストメトリクスが出る

### Task 0.9: CI（cargo check/test/clippy・web build・compose smoke）
- **area**: infra
- **依存**: 0.1
- **path**: `.github/workflows/`
- **仕様**:
  - GitHub Actions: `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`, `web` のlint/build。
  - **compose smoke test**: composeを起動し `/healthz`/`/me`（テストトークン）まで通す統合ジョブ。
  - キャッシュ（cargo, pnpm）でCIを高速化。
- **受け入れ条件**:
  - [ ] PRでlint/test/buildが走り、失敗が赤になる
  - [ ] composeスモークがCIで成功する

### Task 0.10: skillex 連携を見据えた realm/トークン設計＋発行確認
- **area**: auth
- **依存**: 0.4
- **path**: `deploy/keycloak/`, `docs/`
- **仕様**:
  - shiki Keycloak を**共有アイデンティティプール**として設計する方針を確定・文書化
    （skillexはこのrealmへフェデレート、ユーザープール共有、認可は shiki が保持）。
  - skillex の **DLC/LLM 利用に必要なトークン発行**（client/scope/audience）の設計を起こし、
    skillex 用の client を1つ定義してトークン取得まで確認（実利用は skillex 側で）。
  - skillex は並行進行中のため、**この設計が skillex 側をブロックしない**ことを確認。
- **受け入れ条件**:
  - [ ] skillex 用 client でアクセストークンが取得できる
  - [ ] トークンの aud/scope が skillex の想定と一致する旨を文書化
  - [ ] 共有プール/認可分担の設計が `docs/` に残る
