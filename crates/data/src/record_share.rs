//! レコードの個別共有（Task 9.3・スパースタプル）。
//!
//! 「全行を FGA タプルにしない」原則の唯一の例外。行ポリシーで見えない特定レコードを
//! 明示共有した相手にだけ**追加で**見せる（拒否は表現しない・タプル数は共有件数に比例）。
//! 共有できるのは**テーブル owner** または**そのレコードの作成者**（かつ本人に可視な行のみ）。
//! 共有/解除はキャッシュ世代を進め、剥奪を即時反映する（PIT-18 のキャッシュと対）。

use authz::{AuthContext, Consistency, Relation};
use serde::{Deserialize, Serialize};
use serde_json::json;
use storage::audit::{AuditEntry, Decision};
use storage::ShareTarget;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::store::DataStore;
use crate::DataError;

/// 個別共有の役割（共有語彙は viewer/editor のみ・owner の横展開は防ぐ）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RecordShareRole {
    Viewer,
    Editor,
}

impl RecordShareRole {
    fn relation(self) -> Relation {
        match self {
            RecordShareRole::Viewer => Relation::Viewer,
            RecordShareRole::Editor => Relation::Editor,
        }
    }
}

impl DataStore {
    /// レコードを個別共有する（冪等）。
    pub async fn share_record(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        record_id: Uuid,
        target: &ShareTarget,
        role: RecordShareRole,
        trace_id: Option<&str>,
    ) -> Result<(), DataError> {
        self.require_record_share_right(ctx, table_id, record_id, "data.record.share", trace_id)
            .await?;
        let obj = ctx.ns().data_record(&record_id.to_string());
        let subject = target.subject(&ctx.ns());
        self.authz
            .write_tuple(&subject, role.relation(), &obj)
            .await
            .map_err(|e| DataError::Internal(format!("share tuple: {e}")))?;
        // 共有集合キャッシュを世代失効（付与の即時反映）。
        self.material_cache.invalidate();
        self.record_share_audit(
            ctx,
            "data.record.share",
            record_id,
            trace_id,
            json!({ "table_id": table_id, "target": target, "role": role }),
        )
        .await
    }

    /// 個別共有を解除する（冪等・即時反映）。
    pub async fn unshare_record(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        record_id: Uuid,
        target: &ShareTarget,
        role: RecordShareRole,
        trace_id: Option<&str>,
    ) -> Result<(), DataError> {
        self.require_record_share_right(ctx, table_id, record_id, "data.record.unshare", trace_id)
            .await?;
        let obj = ctx.ns().data_record(&record_id.to_string());
        let subject = target.subject(&ctx.ns());
        self.authz
            .delete_tuple(&subject, role.relation(), &obj)
            .await
            .map_err(|e| DataError::Internal(format!("unshare tuple: {e}")))?;
        self.material_cache.invalidate();
        self.record_share_audit(
            ctx,
            "data.record.unshare",
            record_id,
            trace_id,
            json!({ "table_id": table_id, "target": target, "role": role }),
        )
        .await
    }

    /// 共有権限の検査: テーブル owner または当該レコードの作成者（本人に可視な行のみ）。
    async fn require_record_share_right(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        record_id: Uuid,
        action: &str,
        trace_id: Option<&str>,
    ) -> Result<(), DataError> {
        // 前段: テーブル viewer（第1層）＋行可視性（第2層）。不可視は 404（オラクルなし）。
        self.require(ctx, table_id, Relation::Viewer, action, trace_id)
            .await?;
        let table = self.fetch_live(ctx, table_id).await?;
        let row = self
            .select_visible_by_id(ctx, &table, record_id)
            .await?
            .ok_or(DataError::NotFound)?;
        if row.owner == ctx.principal.id {
            return Ok(());
        }
        let is_table_owner = self
            .authz
            .check(
                &ctx.subject(),
                Relation::Owner,
                &ctx.ns().data_table(&table_id.to_string()),
                Consistency::HigherConsistency,
            )
            .await
            .map_err(|e| DataError::Internal(e.to_string()))?;
        if is_table_owner {
            return Ok(());
        }
        let _ = self
            .audit
            .record(
                ctx,
                AuditEntry {
                    action,
                    object_type: "data_record",
                    object_id: &record_id.to_string(),
                    decision: Decision::Deny,
                    trace_id,
                    metadata: json!({ "table_id": table_id }),
                },
            )
            .await;
        Err(DataError::Forbidden)
    }

    async fn record_share_audit(
        &self,
        ctx: &AuthContext,
        action: &str,
        record_id: Uuid,
        trace_id: Option<&str>,
        metadata: serde_json::Value,
    ) -> Result<(), DataError> {
        self.audit
            .record(
                ctx,
                AuditEntry {
                    action,
                    object_type: "data_record",
                    object_id: &record_id.to_string(),
                    decision: Decision::Allow,
                    trace_id,
                    metadata,
                },
            )
            .await
            .map_err(|e| DataError::Internal(format!("audit: {e}")))
    }
}
