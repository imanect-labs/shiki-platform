//! アプリケーション共有状態。

use std::sync::Arc;

use authz::AuthzClient;
use sqlx::PgPool;
use storage::{DirectoryStore, StorageService, TenantStore};

use crate::{config::AppConfig, middleware::JwksCache, session::SessionStore};

/// readiness 疎通確認**専用**の DB ハンドル（#91 M-2）。
///
/// `AppState` に生の `PgPool` を公開すると、ハンドラが `StorageService`
/// （tenant/org スコープ・OpenFGA check・監査の単一チョークポイント）を迂回した
/// 生 SQL を書けてしまう。ping 以外の操作を持たない newtype で包み、
/// 「チョークポイント経由」を規約ではなく型で守る。
#[derive(Clone)]
pub struct ReadinessProbe(PgPool);

impl ReadinessProbe {
    pub fn new(pool: PgPool) -> Self {
        ReadinessProbe(pool)
    }

    /// Postgres へ `SELECT 1` を投げて疎通確認する（readyz 専用）。
    pub async fn ping(&self) -> Result<(), sqlx::Error> {
        sqlx::query("SELECT 1").execute(&self.0).await.map(|_| ())
    }
}

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<AppConfig>,
    /// readiness 疎通確認専用（生 `PgPool` は公開しない。データアクセスは
    /// `storage` / `directory` / `tenants` のチョークポイント経由のみ）。
    pub db: ReadinessProbe,
    /// 認可チョークポイント（具象でなくトレイト経由）。
    pub authz: Arc<dyn AuthzClient>,
    pub jwks: Arc<JwksCache>,
    /// BFF セッションストア（チョークポイント。Redis 実装をトレイト裏に隠す）。
    pub sessions: Arc<dyn SessionStore>,
    /// OIDC backchannel（token 交換・refresh）用の共有 HTTP クライアント。
    pub http: reqwest::Client,
    /// ストレージの単一チョークポイント（権限・監査・content-addressing）。
    pub storage: Arc<StorageService>,
    /// アーティファクト共通枠（Task 6.1・バージョン付き共有本文の単一チョークポイント）。
    pub artifacts: Arc<artifact::ArtifactStore>,
    /// 構造化データサービス（Task 9.2/9.5・テーブル ReBAC＋サーバ検証＋リビジョンの
    /// 単一チョークポイント。行レベル述語は Task 9.3 でここに載る）。
    pub data: Arc<data::DataStore>,
    /// 保存ビュー（Task 9.4・artifact kind=data_view の上に載る・実行は data チョークポイント経由）。
    pub data_views: Arc<data::DataViewStore>,
    /// FSM 定義の保存/解決（Task 9.10・artifact kind=fsm の上・遷移は data チョークポイント）。
    pub fsms: Arc<data::FsmStore>,
    /// コードベース・ミニアプリのマニフェスト/publish（Task 9.1/9.13a）。
    pub mini_app_code: Arc<app_platform::MiniAppCodeStore>,
    /// UI スペックの保存/取得（Task 6.3・artifact kind=ui_spec の上に保存時検証を載せる）。
    pub ui_specs: Arc<gui::UiSpecStore>,
    /// 宣言的 UI アクションの実行系（Task 6.5・照合/本人認可/監査の合流点）。
    pub ui_actions: Arc<gui::ActionDispatcher>,
    /// skill の保存/取得（Task 6.7・artifact kind=skill の上に保存時検証を載せる）。
    pub skills: Arc<gui::SkillStore>,
    /// ミニアプリの保存/解決（Task 6.10・部品はバンドル権限で読む）。
    pub mini_apps: Arc<gui::MiniAppStore>,
    /// シークレット管理（Task 10.9）。マスターキー未設定では `None`（/secrets は 503）。
    pub secrets: Option<Arc<secrets::SecretStore>>,
    /// ワークフロー IR の保存/取得（Task 10.1a・artifact kind=workflow の上に載る）。
    pub workflows: Arc<workflow_engine::WorkflowStore>,
    /// ワークフロー run 起動（対話トリガ・Stage A W3）。`workflow.enabled=false` では `None`。
    pub workflow_launcher: Option<Arc<workflow_engine::WorkflowRunLauncher>>,
    /// ワークフロー run/step 状態取得（実行履歴・Stage A W3）。`workflow.enabled=false` では `None`。
    pub workflow_runs: Option<Arc<workflow_engine::RunStore>>,
    /// ワークフロー有効化・同意・トリガ実体化（Task 10.4a・runtime 無効でも操作可能）。
    pub workflow_registration: Arc<workflow_engine::RegistrationService>,
    /// ワークフロー一覧の要約射影（Task 10.14・認可済み id 集合への単一 SQL）。
    pub workflow_summaries: Arc<workflow_engine::WorkflowSummaryStore>,
    /// dnd エディタのレイアウト永続化（Task 10.12・IR 外・非バージョン）。
    pub workflow_layout: Arc<workflow_engine::EditorLayoutStore>,
    /// API 層の監査レコーダ（有効化/無効化等の管理操作を 1 操作 1 行で記録）。
    pub audit: Arc<storage::audit::AuditRecorder>,
    /// ユーザーディレクトリ（共有相手検索。tenant_id スコープ）。
    pub directory: Arc<DirectoryStore>,
    /// テナントレジストリ（プロビジョニング/削除のライフサイクル・SAAS.2）。
    pub tenants: Arc<TenantStore>,
    /// permission-aware 検索（Phase 2）。`rag.enabled=false` では `None`（/search は 503）。
    pub search: Option<Arc<rag::SearchService>>,
    /// チャット（Phase 3）。`chat.enabled=false` では `None`（/threads 系は 503）。
    /// 生成ワーカーは別途 spawn 済み。ここは API（CRUD/SSE/post/cancel/share）が使うストア。
    pub chat: Option<Arc<chat::ChatStore>>,
    /// RAG のテナント消去（rag_chunk/jobq/Qdrant/Tantivy）。テナント削除フローから呼ぶ。
    /// RAG 無効構成でも DB 行の消去は行う（過去に有効だった残骸を残さない）。
    pub rag_admin: Arc<rag::RagAdmin>,
}
