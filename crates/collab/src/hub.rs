//! プロセス内ドキュメントハブ（ロード・共有・アンロードと authz ゲート）。
//!
//! authz はドキュメント単位＝対応する node の ReBAC（`file:<id>` の viewer/editor）。
//! 接続時に必ず [`CollabHub::authorize`] を通し、セッション中も定期再チェックする
//! （PIT-37②: 接続時 1 回だと共有解除後も update が流れ続ける）。剥奪の即時反映が
//! 要る経路のため整合性は常に `HigherConsistency`（PIT-11 と同じ扱い）。

use std::collections::HashMap;
use std::sync::Arc;

use authz::{AuthContext, AuthzClient, Consistency, Relation};
use storage::{NodeKind, StorageService};
use uuid::Uuid;

use crate::doc::LiveDoc;
use crate::error::CollabError;
use crate::store::DocStore;

/// 接続主体に許すアクセスモード。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessMode {
    /// 読み書き（update の適用を許可）。
    Editor,
    /// 読み取りのみ（update は受理しない・awareness は許可）。
    Viewer,
}

/// セッション中の権限再チェック間隔の既定値（PIT-37②・WOPI と同じ「定期再チェック」）。
const DEFAULT_RECHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

/// ライブドキュメントの共有ハブ。1 プロセスに 1 つ（AppState 常駐）。
pub struct CollabHub {
    docs: tokio::sync::Mutex<HashMap<Uuid, Arc<LiveDoc>>>,
    store: DocStore,
    authz: Arc<dyn AuthzClient>,
    storage: Arc<StorageService>,
    recheck_interval: std::time::Duration,
}

impl CollabHub {
    pub fn new(
        pool: sqlx::PgPool,
        authz: Arc<dyn AuthzClient>,
        storage: Arc<StorageService>,
    ) -> Self {
        CollabHub {
            docs: tokio::sync::Mutex::new(HashMap::new()),
            store: DocStore::new(pool),
            authz,
            storage,
            recheck_interval: DEFAULT_RECHECK_INTERVAL,
        }
    }

    /// 権限再チェック間隔を差し替える（テストで剥奪切断を短時間に検証するため）。
    #[must_use]
    pub fn with_recheck_interval(mut self, interval: std::time::Duration) -> Self {
        self.recheck_interval = interval;
        self
    }

    pub fn recheck_interval(&self) -> std::time::Duration {
        self.recheck_interval
    }

    pub fn store(&self) -> &DocStore {
        &self.store
    }

    /// 接続時・定期再チェック共通の認可判定（fail-closed）。
    ///
    /// editor → 読み書き、viewer → 読み取りのみ、どちらも無ければ Forbidden。
    pub async fn authorize(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
    ) -> Result<AccessMode, CollabError> {
        let object = ctx.ns().file(&node_id.to_string());
        let subject = ctx.subject();
        if self
            .authz
            .check(
                &subject,
                Relation::Editor,
                &object,
                Consistency::HigherConsistency,
            )
            .await?
        {
            return Ok(AccessMode::Editor);
        }
        if self
            .authz
            .check(
                &subject,
                Relation::Viewer,
                &object,
                Consistency::HigherConsistency,
            )
            .await?
        {
            return Ok(AccessMode::Viewer);
        }
        Err(CollabError::Forbidden(format!("file {node_id}")))
    }

    /// ノード実在＋ファイル種別の確認（viewer チェック・監査込み＝StorageService 経由）。
    pub async fn require_file(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<storage::Node, CollabError> {
        let node = self.storage.get_metadata(ctx, node_id, trace_id).await?;
        if node.kind != NodeKind::File {
            return Err(CollabError::NotFound(format!("file {node_id}")));
        }
        Ok(node)
    }

    /// ドキュメントに参加する（未ロードなら永続状態から復元）。
    ///
    /// 認可済みであること（[`Self::authorize`]）を呼び出し側の責務とする。
    pub async fn join(
        &self,
        node_id: Uuid,
        org: &str,
        tenant_id: &str,
    ) -> Result<Arc<LiveDoc>, CollabError> {
        let mut docs = self.docs.lock().await;
        let doc = if let Some(doc) = docs.get(&node_id) {
            Arc::clone(doc)
        } else {
            let persisted = self.store.load_or_init(node_id, org, tenant_id).await?;
            let live = Arc::new(LiveDoc::restore(node_id, &persisted)?);
            docs.insert(node_id, Arc::clone(&live));
            live
        };
        doc.conn_joined();
        Ok(doc)
    }

    /// ドキュメントから離脱する。最終接続なら最終圧縮してアンロードする。
    pub async fn leave(&self, doc: &Arc<LiveDoc>) {
        if doc.conn_left() > 0 {
            return;
        }
        let mut docs = self.docs.lock().await;
        // ロック取得までの間に新規参加があり得るため、ロック下で再確認する。
        if doc.conn_count() == 0 {
            docs.remove(&doc.node_id);
            drop(docs);
            if let Err(e) = doc.compact_now(&self.store).await {
                tracing::warn!(node_id = %doc.node_id, error = %e, "最終圧縮に失敗（log は保全済み）");
            }
        }
    }
}
