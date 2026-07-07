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
