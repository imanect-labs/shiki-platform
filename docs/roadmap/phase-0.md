# Phase 0 — 歩く骨格（Walking Skeleton）

> 目的: トレイト境界・認証/認可・**SaaS トポロジ**・配布形態（compose）・型契約・可観測性の土台を**最初に1本通す**。
> 機能価値はまだ無いが、以降の全フェーズがこの骨格に乗る。
> **SaaS を優先ターゲット**とし、shiki の SaaS コントロールプレーン＋顧客ごと隔離 cell データプレーン（design §4.1.1）と `tenant_id` を day-1 から前提にする。認証は **BFF + オパークセッション Cookie**（Redis）。
> 完了の定義(DoD): `docker compose up` 一発で全依存（Redis 含む）が起動し、Keycloak でログインしたユーザーが
> **セッション Cookie** で OpenFGA 認可される `GET /me`（`tenant_id` スコープ込み）をブラウザから叩けて、その1リクエストが OTel トレースに現れる。
> トレースを見るには監視スタックを `observability` profile で起動する（`OTLP_ENDPOINT=http://otel-collector:4317 docker compose --profile observability up -d`）。
> （旧: 監視スタックも既定の `docker compose up` で起動 → 置換: `observability` profile でオプトイン起動（開発機の RAM 節約）・根拠 README.md / `deploy/compose/docker-compose.yml` 冒頭）

> **実装突合（2026-10-17・main 329aba0）**: 0.1〜0.11 の主要経路（compose 起動 → Keycloak ログイン → BFF のセッション Cookie → OpenFGA check 付き `GET /me`）は実装済み。CI の compose smoke（`smoke-bff.sh`）・Web E2E（`auth.spec.ts`）・live OpenFGA の authz 結合テストで毎回実走し、main は緑。残り: web に OpenAPI 登録済み型の手書き重複がある #631／ログが Loki に届かず、span が単一で traceparent も取り込まない #632／骨格のテスト欠落（/readyz 503・JWKS ローテ・/me 403・OTel 到達）#633／ts-rs 生成物（`crates/*/bindings`）が commit されていてドリフトする #422

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
| 0.11 | BFF + オパークセッション Cookie（Redis セッションストア）へ認証を置換（#55） | auth | 0.2, 0.4 |

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
  - [x] `cargo build` がワークスペース全体で通る
  - [x] `crates/` の各クレートがワークスペースメンバとして認識される
  - [x] `web/` で `pnpm dev`（or npm）が起動する（CI は `pnpm build` ＋ `pnpm start` で起動を確認）

### Task 0.2: docker compose 基盤（依存ミドルウェア）
- **area**: infra
- **依存**: 0.1
- **path**: `deploy/compose/`
- **仕様**:
  - `docker-compose.yml` に Phase 0 で必要な依存を定義: **Postgres**, **Keycloak**, **OpenFGA**, **RustFS**, **Redis**（BFF セッションストア・Task 0.11/#55）。
    （旧: MinIO → 置換: RustFS。MinIO のイメージ配布が止まったため・根拠 #490 / `deploy/compose/MIGRATION-rustfs.md`）
    （Qdrant/Tantivy/ingestion等は後続フェーズで追記）
  - 各サービスにヘルスチェック、永続ボリューム、`.env.example`（接続情報）。
    Redis は永続化しない設定（`--save "" --appendonly no`）で、compose を再起動するとセッションが消えて再ログインになる。
    永続ボリュームを持つのは Postgres・RustFS（後続フェーズで Qdrant 等も）。
  - OpenFGA は Postgres をバックエンドに設定。Keycloak も Postgres を使用。
  - shiki-server もサービスとして追加（後で 0.3 のイメージを参照）。
- **受け入れ条件**:
  - [x] `docker compose up` で全サービスが healthy になる（CI の compose smoke は shiki-server の `depends_on: service_healthy` で Postgres/Keycloak/OpenFGA/Redis/RustFS の healthy を待つ）
  - [x] RustFS（旧 MinIO）/Keycloak/OpenFGA の管理UI/エンドポイントに到達できる（CI で到達するのは各エンドポイントまで。RustFS コンソール :9001 などの管理 UI は手動確認）
  - [ ] 再起動してもデータが永続する（Postgres・RustFS は名前付きボリュームで永続。Redis は非永続で、再起動するとセッションが消える。再起動後の永続を確かめるテストは無い）

### Task 0.3: axum サーバ雛形＋設定ローダ＋ヘルスチェック
- **area**: infra
- **依存**: 0.1
- **path**: `crates/api`
- **仕様**:
  - axum でHTTPサーバを起動。`GET /healthz`（liveness）, `GET /readyz`（依存接続確認）。
  - **設定ローダ**: 環境変数/設定ファイルから読み、`AppConfig` に集約。
    クラウド/オンプレ差し替えの起点として、各トレイト実装の選択を設定で切替える前提の構造にする
    （例: `storage.backend = "s3" | "gcs"`）。Phase 0 では値の読み込みと検証のみ。
  - Postgres 接続プール（sqlx等）を初期化し `/readyz` で疎通確認。
  - 構造化ログ（tracing）の初期化（OTelは0.8で接続）。
- **受け入れ条件**:
  - [x] `/healthz` が200を返す
  - [ ] `/readyz` がPostgres断時に503、復帰で200（実装済み・検証テスト欠落 #633）
  - [x] 設定の必須欠落時に起動エラーで明確に落ちる

### Task 0.4: Keycloak realm 構成＋OIDC 検証ミドルウェア（土台）
- **area**: auth
- **依存**: 0.2, 0.3
- **path**: `crates/api`（middleware）, `deploy/keycloak/`
> ⚠️ **認証方式は BFF + オパークセッション Cookie に確定**（ADR `docs/auth/browser-token-strategy.md` / Task 0.11 #55）。本タスクは Keycloak realm と**クレーム抽出→`principal`** までを土台として作り、`Authorization: Bearer` 検証は **Task 0.11 でセッション Cookie 検証へ置換**される（Bearer 版を最終形にしない・ブラウザにトークンを持たせる前提を残さない）。
- **仕様**:
  - Keycloak に `shiki` realm を定義（realm export JSON を `deploy/keycloak/` に commit、起動時インポート）。
    フロント用 client（BFF の Authorization Code + PKCE 用・confidential）、API用設定、テストユーザーを含む。
  - **クレーム抽出（再利用される中核）**: 検証済み OIDC クレームから `principal`（user id, email, groups, roles）を抽出し request extension に載せる（`claims.rs`）。この層は 0.11 でも再利用する。
    （旧: groups/dept → 置換: groups ＋ roles（claim `roles`・多値。`Principal.roles`））
  - **トークン検証ロジック**（JWKS取得・キャッシュ・署名/exp/aud/iss検証）は、0.11 では BFF の token 交換後の ID/Access token 検証として再利用する。`Authorization: Bearer` 入口は 0.11 で撤去。
  - SSE は Cookie 自動添付を前提にする（ヘッダ認証は不要。POST ストリームは Task 3.5 の方式に従う）。
  - 顧客IdPフェデレーション（AD/Entra/Okta）は**設定で追加できる構造**にするが、Phase 0 では shiki realm のみ。
- **受け入れ条件**:
  - [x] 有効なトークンで保護エンドポイントにアクセスでき、無効/期限切れは401（Bearer 入口は 0.11 で撤去。現在は BFF callback での token 検証（未知 kid・aud 不一致は 401）とセッション Cookie で検証する）
  - [ ] JWKSのローテーションに追従（キャッシュTTL／kid不一致で再取得）（実装済み・検証テスト欠落 #633）
  - [x] principal にユーザーIDと所属group/rolesが入る（旧: group/dept）

### Task 0.5: OpenFGA 配線＋authzクライアント＋認可コンテキスト
- **area**: auth
- **依存**: 0.2, 0.3
- **path**: `crates/authz`
- **仕様**:
  - OpenFGA クライアントクレートを実装（store作成、authorization model のロード/バージョン管理）。
  - **最小 relation model** を定義（Phase 0 は骨格のみ）: `organization`, `role`, `user`,
    relation `member`。ロール階層は `role#member` のネスト（`[user, role#member]`）で**配下ロール→親ロールへ
    上方向ロールアップ**する（親ロールは配下ロールのメンバーを含む）。`parent` relation は Phase 1 で
    `folder`/`file` 追加時に導入する。後続フェーズで `thread`/`doc_chunk` を追加する前提のスキーマ構成。
  - **認可コンテキスト** `AuthContext { principal, org, tenant_id }` を定義し、全データアクセスがこれを受け取る規約を導入
    （**SaaS マルチテナント前提で `tenant_id` を day-1 から保持**・後付けで隔離境界を壊さない）。`check(user, relation, object)` ヘルパを提供。
  - **`tenant_id` の取得元（human 決定 / #57）= 案C 既定 ＋ 案A 継ぎ目**: 解決は `crates/api` の
    `resolve_tenant_id` 一点に集約し、設定 `auth.tenancy`（`single`/`multi`）でモード分岐する:
    - `single`（案C・オンプレ/cell・既定）: 設定 `auth.tenant_id` の固定値（既定 `"default"`）。
      defaults で固定値が効くため無設定でも後方互換で動作する（固定値が空なら設定ミスとして拒否）。
    - `multi`（案A・SaaS）: Keycloak claim `tenant` を**必須**にし、欠落・空白は **fail-closed で拒否**
      （固定値へフォールバックして無関係なテナントへ黙って融合させない）。
    取得元は state を持つ認証境界（`require_auth` / #55 のセッション middleware）で解決し extension 経由で
    `AuthContext` 構築に渡す（extractor は state 非依存を維持）。
  - **実分離の強制は SAAS.1**: OpenFGA の subject/object 識別子（`user:<id>` / `organization:<org>`）を
    `tenant_id` でスコープ化する強制は roadmap トラック SAAS.1 の責務。本タスクは `AuthContext` に
    `tenant_id` を保持する継ぎ目の用意までを範囲とする（`subject()` の doc にも明記）。
  - relation model は `docs/design.md` の ReBAC 図に準拠。**model定義は人がレビュー**（ポリシ決定）。
- **受け入れ条件**:
  - [x] authorization model が OpenFGA にロードされバージョンが記録される（`ensure_store_and_model` が model_id を固定・ログ出力）
  - [x] `check()` が tuple に基づき allow/deny を返す（ユニットテスト）（`crates/authz/tests/check.rs` は live OpenFGA の結合テストで、CI の compose smoke と Coverage で `OPENFGA_TEST_URL` を渡して実走）
  - [x] AuthContext を経由しないデータアクセスがコンパイル/レビューで弾ける設計になっている

### Task 0.6: 認証＋認可付きサンプルエンドポイント `GET /me` をE2E
- **area**: api
- **依存**: 0.4, 0.5
- **path**: `crates/api`
- **仕様**:
  - `GET /me`: セッション Cookie → principal 取得 → OpenFGA で簡単な check（例: 自分のorgのmemberか）→
    ユーザー情報（id, email, roles, org, tenant_id, is_admin）をJSONで返す。
    （旧: JWT 検証 → 置換: セッション Cookie（Task 0.11）。旧: dept → 置換: roles）
  - 認証ミドルウェア・authzクライアント・設定・DBが1リクエストで協調する**縦の最小貫通**を確立。
  - OpenAPI（utoipa）注釈を付け、0.7の型生成対象にする。
- **受け入れ条件**:
  - [x] ログイン済みフロントから `/me` が自分の情報を返す
  - [x] 別orgリソースへのcheckがdenyされる負例テスト（authz 層の `check_allows_member_and_denies_other_org`。HTTP 層で `/me` が 403 になるテストは欠落 #633）
  - [ ] このリクエストが0.8のトレースに1本のspanツリーとして現れる（実装済み・検証テスト欠落 #633。span はリクエストのルート 1 本だけで、子 span が無い #632）

### Task 0.7: Next.js 雛形＋OIDCログイン＋型生成パイプライン
- **area**: frontend
- **依存**: 0.3, 0.4
- **path**: `web/`
> ⚠️ **ブラウザにトークンを保持しない**（BFF + オパークセッション Cookie に確定・ADR / Task 0.11 #55）。OIDC の code 受け／token 交換／refresh は**サーバ側（BFF）**が担い、フロントは `localStorage` 保持・silent renew・`Authorization` ヘッダ自動付与を**実装しない**。
- **仕様**:
  - Next.js App Router 雛形、OIDCログイン（Keycloak へリダイレクト → **BFF が code/token 交換**。ブラウザはトークンを保持せず**セッション Cookie のみ**）。
  - ログイン後に `/me` を呼んで表示する最小画面。
  - **型生成パイプライン**: `utoipa` が出すOpenAPI仕様 → `openapi-typescript` でTSクライアント/型生成（`pnpm gen:api` = `scripts/gen-api.sh`）。
    SSE イベント型・content-block も utoipa `ToSchema` → OpenAPI → openapi-typescript で生成する（`web/src/lib/chat-api.ts`）。
    認可語彙・ワークフロー語彙/IR・GUI スペック型は ts-rs で生成する。
    （旧: SSE イベント型は ts-rs/typeshare → 置換: OpenAPI 経由）
    生成物は commit せず CI/スクリプトで再生成可能に（`web/src/generated/` は `.gitignore` 済み。ただし ts-rs の既定出力 `crates/*/bindings/*.ts` は commit されている #422）。
  - 認証付き fetch ラッパ（**`credentials:'include'` でセッション Cookie 送出**。SSE は Cookie 自動添付。POST ストリームは Task 3.5 方式）。
- **受け入れ条件**:
  - [x] ブラウザでログイン→`/me`の自分情報が表示される
  - [x] `pnpm gen:api`（等）でRust定義から型/クライアントが再生成される
  - [ ] 手書きのAPI型が存在しない → #631（OpenAPI 登録済みの `AutonomousMode`・`ThreadRole`・`ShareTarget`・`ArtifactRole`・`ArtifactKind` を web が手書きで重複定義している）

### Task 0.8: OTel 計装の土台
- **area**: obs
- **依存**: 0.3
- **path**: `crates/api`（telemetry init）, `deploy/compose/`
- **仕様**:
  - `tracing` + `opentelemetry` を初期化し、OTLPでエクスポート。compose に **Grafana/Tempo/Loki/Prometheus**
    を最小構成で追加（オンプレ既定）。監視スタックは `observability` profile でオプトイン起動する。クラウドはエクスポータ差し替え前提の抽象に。
  - axum リクエストに span を張り、trace_id を伝播（後で Langfuse と相関させる種）。
  - 基本メトリクス（リクエスト数/レイテンシ）とログのtrace_id付与。
    shiki-server が OTLP で送るのは trace と metric だけで、ログは stdout のみ。collector の logs パイプライン（→ Loki）には何も届かない #632。
- **受け入れ条件**:
  - [ ] `/me` 1回が Tempo でトレースとして閲覧できる（実装済み・検証テスト欠落 #633）
  - [ ] ログに trace_id が含まれ、トレースと突合できる → #632（stdout のログ行には trace_id が載るが、Loki に届かない）
  - [ ] Prometheus にリクエストメトリクスが出る（実装済み・検証テスト欠落 #633）

### Task 0.9: CI（cargo check/test/clippy・web build・compose smoke）
- **area**: infra
- **依存**: 0.1
- **path**: `.github/workflows/`
- **仕様**:
  - GitHub Actions: `cargo fmt --check`, `cargo clippy -D warnings`, `cargo nextest run --workspace --all-features`, `web` のlint/build。
    （旧: `cargo test` → 置換: `cargo nextest run`・`.github/workflows/ci.yml`）
  - **compose smoke test**: composeを起動し `/healthz`/`/me` まで通す統合ジョブ。
    テストユーザー（alice）で BFF の code フローを curl で駆動し、セッション Cookie で `/me` を叩く（`deploy/compose/smoke-bff.sh`）。
    （旧: テストトークン → 置換: BFF code フロー（Task 0.11））
  - キャッシュ（cargo, pnpm）でCIを高速化。
  - **コード品質ゲート（AI生成の品質劣化対策）**:
    - clippy 厳格化（`[workspace.lints]`）: 安全系（`unwrap_used`/`expect_used`/`panic`/
      `dbg_macro`/`print_*` 等）は deny、`pedantic` は本番コードに適用（テスト/CLI bin は
      方針に沿ってスコープ許容）。しきい値は `clippy.toml`。
    - **ファイルサイズ gate**: `scripts/check-file-size.sh`（src の 1 ファイル 1000 行以内）。
    - **cargo-machete**: 未使用依存の検出。
    - **cargo-deny**: 脆弱性(RUSTSEC)/ライセンス/出所/wildcard 依存を `deny.toml` で検査。
- **受け入れ条件**:
  - [x] PRでlint/test/buildが走り、失敗が赤になる
  - [x] composeスモークがCIで成功する
  - [x] clippy厳格化・ファイルサイズgate・cargo-machete・cargo-denyが緑（#品質ゲート）

### Task 0.10: skillex 連携を見据えた realm/トークン設計＋発行確認
- **area**: auth
- **依存**: 0.4
- **path**: `deploy/keycloak/`, `docs/`
- ⚠️ **2026-09-29 更新**: 下記の「共有アイデンティティプール」方針は**廃止**（ユーザー認証は SaaS でも分離）。
  残るのは `skillex` client の client_credentials（`aud=shiki-llm`）による llm-gateway の m2m 利用のみ
  （design §4.1.1・parallel-tracks SK・`docs/auth/skillex-identity.md`）。`aud=shiki-llm` のトークンを受け付けるエンドポイントは
  parallel-tracks SK.8 の範囲で、本フェーズには含まない。以下は経緯として残す。
- **仕様**:
  - shiki Keycloak を**共有アイデンティティプール**として設計する方針を確定・文書化
    （skillexはこのrealmへフェデレート、ユーザープール共有、認可は shiki が保持）。
  - skillex の **DLC/LLM 利用に必要なトークン発行**（client/scope/audience）の設計を起こし、
    skillex 用の client を1つ定義してトークン取得まで確認（実利用は skillex 側で）。
  - skillex は並行進行中のため、**この設計が skillex 側をブロックしない**ことを確認。
- **受け入れ条件**:
  - [x] skillex 用 client でアクセストークンが取得できる（CI の compose smoke で取得し、`aud` に `shiki-llm` が含まれることを assert）
  - [x] トークンの aud/scope が skillex の想定と一致する旨を文書化（`docs/auth/skillex-identity.md`）
  - [x] 認可分担（ユーザー認証は分離・残る結合は m2m のみ）の設計が `docs/` に残る（旧: 共有プール/認可分担の設計 → 置換: 2026-09-29 に共有プールを廃止・`docs/auth/skillex-identity.md`）

### Task 0.11: BFF + オパークセッション Cookie（Redis セッションストア）へ認証を置換
- **area**: auth
- **依存**: 0.2, 0.4
- **path**: `crates/api`（`routes/auth/`・`middleware/session.rs`・`session/`）, `web/`, `deploy/compose/`
- **Issue**: #55
- **根拠**: ADR `docs/auth/browser-token-strategy.md`（推奨構成 (A)・2026-06-18 human 合意）
- **仕様**:
  - **ブラウザ ⇄ api はオパークセッション Cookie のみ**: セッション Cookie `shiki_session`（httpOnly・値は `{session_id}.{tenant_id}`）と
    CSRF Cookie `shiki_csrf`（JS から読める）を発行する。トークンはブラウザに置かない。
  - **BFF エンドポイント**（`crates/api/src/routes/auth/`）: `GET /auth/login`（PKCE 付き authorize へリダイレクト）／
    `GET /auth/callback`（サーバ側で code → token 交換・JWKS でトークン検証・セッション発行）／`POST /auth/logout`／
    `GET /auth/session`／`POST /auth/backchannel-logout`（OIDC Back-Channel Logout）。
  - **セッションミドルウェア**（`middleware/session.rs`）: Cookie → セッション → `Principal` ＋ `tenant_id` を extension に載せる。
    access token の期限前にサーバ側で refresh ローテーションし、refresh も失効していればセッションを破棄して 401。
    状態変更系メソッドは double-submit CSRF を検証する。
  - **セッションストア**: `SessionStore` トレイトの裏に Redis 実装（`session/redis_store.rs`）。キーは `tenant_id` でスコープする。
    compose の Redis は永続化しない設定のため、再起動するとセッションが消えて再ログインになる。
  - **web**: `apiFetch` は `credentials: "include"` で Cookie を送り、`Authorization` ヘッダは使わない（`web/src/lib/api.ts`）。
  - api ⇄ 内部/サービス間は従来どおり JWT / token-exchange（ADR §4）。
- **受け入れ条件**:
  - [x] ブラウザにトークンを置かない（セッション Cookie が JWT 形状でないことを `smoke-bff.sh` が検証）
  - [x] login → callback → セッション Cookie で `/me` が 200、Cookie 無しは 401（`smoke-bff.sh`・`crates/api/tests/auth_flow.rs`・`crates/api/tests/http.rs`）
  - [x] access token の期限前に refresh で継続し、refresh できなければ 401（`expired_access_token_is_refreshed_and_continues`・`expired_access_without_refresh_is_unauthorized`）
  - [x] セッション削除・Back-Channel Logout で即時失効する（`revoked_session_is_immediately_unauthorized`・`backchannel_logout_*`）
  - [x] CSRF トークン無しの状態変更は拒否される（`logout_without_csrf_is_forbidden`）
  - [x] セッションキーが `tenant_id` でスコープされる（`session_store_is_tenant_scoped`・`crates/api/tests/redis_session.rs` は CI の Coverage で `REDIS_TEST_URL` を渡して実走）
