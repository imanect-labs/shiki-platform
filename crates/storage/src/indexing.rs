//! インデクサ（RAG インジェスト・Task 2.8）専用のストレージ・ファサード。
//!
//! # なぜユーザー `AuthContext` を取らないのか
//!
//! インジェストは outbox イベント駆動のシステム内部処理で、「どのユーザーの権限で
//! 読むか」が存在しない（文書は全可読者のために一度だけ索引される）。認可の実効境界は
//! 検索時の二段 authz（pre-filter ＋ OpenFGA post-filter・Task 2.7）が担い、本ファサードは
//! **tenant_id スコープの読み取りと内部向け presigned GET の発行**だけを行う。
//! presigned URL の発行経路をここ 1 点に閉じ、単一チョークポイント原則との緊張を
//! 監査可能な範囲に留める（発行は短 TTL・内部エンドポイント署名・worker 専用）。

use std::sync::Arc;
use std::time::Duration;

use sqlx::PgPool;
use uuid::Uuid;

use crate::content_address::blob_object_key;
use crate::error::StorageError;
use crate::object_store::ObjectStore;

/// インジェスト時点のノード状態のスナップショット。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct NodeSnapshot {
    pub id: Uuid,
    pub org: String,
    pub kind: String,
    pub name: String,
    pub version: i64,
    pub blob_sha256: Option<String>,
    pub size_bytes: Option<i64>,
    pub content_type: Option<String>,
    /// 論理削除済みか（削除済みは索引対象外）。
    pub deleted: bool,
}

/// インデクサ専用ファサード。RAG パイプライン以外から使わないこと。
pub struct IndexerStorage {
    pool: PgPool,
    store: Arc<dyn ObjectStore>,
    /// worker が blob を読むための内部 presigned GET の TTL（短命）。
    internal_get_ttl: Duration,
}

impl IndexerStorage {
    pub fn new(pool: PgPool, store: Arc<dyn ObjectStore>) -> Self {
        IndexerStorage {
            pool,
            store,
            internal_get_ttl: Duration::from_mins(1),
        }
    }

    /// ノードの現在状態を読む。存在しなければ `None`（stale イベントの検出に使う）。
    pub async fn node_snapshot(
        &self,
        tenant_id: &str,
        node_id: Uuid,
    ) -> Result<Option<NodeSnapshot>, StorageError> {
        let snapshot = sqlx::query_as::<_, NodeSnapshot>(
            "select id, org, kind, name, version, blob_sha256, size_bytes, content_type, \
                    (deleted_at is not null) as deleted \
             from node where id = $1 and tenant_id = $2",
        )
        .bind(node_id)
        .bind(tenant_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(snapshot)
    }

    /// ノードの祖先フォルダ id 一覧（自身は含まない・ルートまで）。
    ///
    /// authz_tags（PIT-1 (b): `folder:<tenant>|<祖先>` 群）の材料。closure table から
    /// 一回のクエリで引く。
    pub async fn ancestor_folder_ids(
        &self,
        tenant_id: &str,
        node_id: Uuid,
    ) -> Result<Vec<Uuid>, StorageError> {
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "select c.ancestor from node_closure c \
             join node n on n.id = c.ancestor \
             where c.descendant = $1 and c.depth > 0 and n.tenant_id = $2",
        )
        .bind(node_id)
        .bind(tenant_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(ids)
    }

    /// フォルダ配下の**子孫ファイル** (id, 現行 version) 一覧（自身は含まない・深さ不問）。
    ///
    /// フォルダの move/delete/restore イベントを子孫ファイルへ展開する材料
    /// （storage はフォルダ 1 件のイベントしか発行しないため）。
    pub async fn descendant_files(
        &self,
        tenant_id: &str,
        folder_id: Uuid,
    ) -> Result<Vec<(Uuid, i64)>, StorageError> {
        let files: Vec<(Uuid, i64)> = sqlx::query_as(
            "select c.descendant, n.version from node_closure c \
             join node n on n.id = c.descendant \
             where c.ancestor = $1 and c.depth > 0 and n.tenant_id = $2 and n.kind = 'file'",
        )
        .bind(folder_id)
        .bind(tenant_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(files)
    }

    /// worker が blob を読むための**内部向け**短 TTL presigned GET URL を発行する。
    pub async fn presign_internal_get(
        &self,
        tenant_id: &str,
        org: &str,
        blob_sha256: &str,
    ) -> Result<String, StorageError> {
        let key = blob_object_key(tenant_id, org, blob_sha256);
        let url = self
            .store
            .presign_get_internal(&key, self.internal_get_ttl)
            .await?;
        Ok(url)
    }
}
