//! ワークフロー一覧の要約（Task 10.14・一覧 API の単一クエリ射影）。
//!
//! 認可済み id 集合（artifact の所有∪共有・API 層が解決）を受け取り、**単一 SQL** で
//! 最新版 body の表示情報（display_name/description/トリガ種）と registration 状態を射影する。
//! body 全体・IR 本文は運ばない（必要フィールドのみ・パフォーマンス規約）。

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

/// 一覧 1 行の要約。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct WorkflowSummary {
    pub id: Uuid,
    pub name: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub current_version: i64,
    /// IR triggers[] の kind 列（schedule/event/interactive・重複除去なし）。
    pub trigger_kinds: Vec<String>,
    /// enabled / disabled / suspended_reconsent / none（未登録）。
    pub enabled_status: String,
    pub enabled_version: Option<i64>,
    pub updated_at: DateTime<Utc>,
}

/// 一覧要約の読み取り（読み取り専用・認可は呼び出し側で解決済みの id 集合が前提）。
#[derive(Clone)]
pub struct WorkflowSummaryStore {
    db: PgPool,
}

impl WorkflowSummaryStore {
    pub fn new(db: PgPool) -> Self {
        WorkflowSummaryStore { db }
    }

    /// id 集合の要約を更新日降順で返す（id は認可済み前提・tenant 二重フィルタ）。
    pub async fn list(
        &self,
        tenant_id: &str,
        ids: &[Uuid],
    ) -> Result<Vec<WorkflowSummary>, sqlx::Error> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as(
            "SELECT a.id, a.name, \
                    v.body->>'display_name' AS display_name, \
                    v.body->>'description' AS description, \
                    a.current_version, \
                    COALESCE( \
                      (SELECT array_agg(t->>'kind') \
                       FROM jsonb_array_elements(v.body->'triggers') AS t), \
                      '{}') AS trigger_kinds, \
                    COALESCE(r.status, 'none') AS enabled_status, \
                    r.enabled_version, \
                    a.updated_at \
             FROM artifact a \
             JOIN artifact_version v \
               ON v.tenant_id = a.tenant_id AND v.artifact_id = a.id \
              AND v.version = a.current_version \
             LEFT JOIN workflow_registration r \
               ON r.tenant_id = a.tenant_id AND r.workflow_id = a.id \
             WHERE a.tenant_id = $1 AND a.id = ANY($2) AND a.deleted_at IS NULL \
             ORDER BY a.updated_at DESC, a.id DESC",
        )
        .bind(tenant_id)
        .bind(ids)
        .fetch_all(&self.db)
        .await
    }
}
