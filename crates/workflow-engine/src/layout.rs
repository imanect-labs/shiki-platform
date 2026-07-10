//! dnd エディタのレイアウト永続化（Task 10.12・ノード座標は IR 外）。
//!
//! 座標は化粧品（非バージョン・検証不要・共有はワークフロー単位で 1 つ）。IR に入れない理由:
//! deny-unknown / ir_version 据え置き・AI 生成 IR が座標なしで dnd に開ける（dagre 自動配置）。

use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

/// レイアウトの保存/取得（`workflow_editor_layout`・migration 0031）。
#[derive(Clone)]
pub struct EditorLayoutStore {
    db: PgPool,
}

impl EditorLayoutStore {
    pub fn new(db: PgPool) -> Self {
        EditorLayoutStore { db }
    }

    /// レイアウトを取得する（未保存は `{}`）。
    pub async fn get(&self, tenant_id: &str, workflow_id: Uuid) -> Result<Value, sqlx::Error> {
        let row: Option<(sqlx::types::Json<Value>,)> = sqlx::query_as(
            "SELECT layout FROM workflow_editor_layout \
             WHERE tenant_id = $1 AND workflow_id = $2",
        )
        .bind(tenant_id)
        .bind(workflow_id)
        .fetch_optional(&self.db)
        .await?;
        Ok(row.map_or_else(|| Value::Object(serde_json::Map::default()), |(j,)| j.0))
    }

    /// レイアウトを保存する（upsert・256KB 上限で肥大防止）。
    pub async fn put(
        &self,
        tenant_id: &str,
        workflow_id: Uuid,
        layout: &Value,
    ) -> Result<(), LayoutError> {
        let size = serde_json::to_vec(layout).map_or(0, |v| v.len());
        if size > 256 * 1024 {
            return Err(LayoutError::TooLarge);
        }
        sqlx::query(
            "INSERT INTO workflow_editor_layout (tenant_id, workflow_id, layout) \
             VALUES ($1, $2, $3) \
             ON CONFLICT (tenant_id, workflow_id) \
             DO UPDATE SET layout = $3, updated_at = now()",
        )
        .bind(tenant_id)
        .bind(workflow_id)
        .bind(sqlx::types::Json(layout))
        .execute(&self.db)
        .await
        .map_err(LayoutError::Db)?;
        Ok(())
    }
}

/// レイアウト保存のエラー。
#[derive(Debug, thiserror::Error)]
pub enum LayoutError {
    #[error("レイアウトが大きすぎます（256KB 上限）")]
    TooLarge,
    #[error("db: {0}")]
    Db(sqlx::Error),
}
