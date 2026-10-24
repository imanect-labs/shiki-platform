//! レコードの個別共有（Task 9.3・スパースタプル）。
//!
//! 「全行を FGA タプルにしない」原則の唯一の例外。行ポリシーで見えない特定レコードを
//! 明示共有した相手にだけ**追加で**見せる（拒否は表現しない・タプル数は共有件数に比例）。
//! 共有できるのは**テーブル owner** または**そのレコードの作成者**（かつ本人に可視な行のみ）。
//! 共有/解除はキャッシュ世代を進め、剥奪を即時反映する（PIT-18 のキャッシュと対）。

use authz::{AuthContext, Consistency, Relation};
use serde::{Deserialize, Serialize};
use serde_json::json;
use storage::audit::AuditAction;
use storage::audit::{AuditEntry, Decision};
use storage::ShareTarget;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::store::DataStore;
use crate::{map_db, DataError};

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
        self.require_record_share_right(
            ctx,
            table_id,
            record_id,
            AuditAction::DataRecordShare,
            trace_id,
        )
        .await?;
        self.validate_share_target(ctx, target).await?;
        // タプルを書く**前に**「個別共有あり」を立て、このテーブルを参照先にする材料化列を降格する
        // （共有の有無で行集合が変わる＝材料化の前提の第 4 階層が崩れる・PIT-71）。タプルの書込に
        // 失敗しても旗は立ったまま（安全側）。
        self.mark_record_shared(ctx, table_id).await?;
        let obj = ctx.ns().data_record(&record_id.to_string());
        let subject = target.subject(&ctx.ns());
        let wrote = self
            .authz
            .write_tuple(&subject, role.relation(), &obj)
            .await
            .map_err(|e| DataError::Internal(format!("share tuple: {e}")))?;
        // 監査失敗時はタプルを補償（監査なしの共有を残さない・Codex P2）。撤回は
        // 実際に付与した場合のみ（冪等 write の false は既存タプル＝撤回しない）。
        if let Err(e) = self
            .record_share_audit(
                ctx,
                AuditAction::DataRecordShare,
                record_id,
                trace_id,
                json!({ "table_id": table_id, "target": target, "role": role }),
            )
            .await
        {
            if wrote {
                if let Err(c) = self
                    .authz
                    .delete_tuple(&subject, role.relation(), &obj)
                    .await
                {
                    tracing::error!(error = %c, %record_id, "共有監査失敗後のタプル補償削除にも失敗");
                }
            }
            return Err(e);
        }
        self.after_share_change(ctx, table_id).await;
        Ok(())
    }

    /// 共有先の検査: id の形（subject を壊す文字の拒否）と、ユーザーは**同一テナントのディレクトリに
    /// 居ること**（存在しない・他テナントの id への共有タプルを作らない）。ロールは StorageService の
    /// 共有と同じく形だけを見る（IdP 同期前の部署へも共有できるようにするため）。解除は検査しない
    /// （残った不正なタプルを消せるように）。
    async fn validate_share_target(
        &self,
        ctx: &AuthContext,
        target: &ShareTarget,
    ) -> Result<(), DataError> {
        let id = match target {
            ShareTarget::User { id } | ShareTarget::Role { id } => id,
        };
        if id != id.trim() {
            return Err(DataError::Invalid(
                "共有先 id の前後に空白は使えません".into(),
            ));
        }
        authz::validate_local_id(id)
            .map_err(|v| DataError::Invalid(format!("共有先 id が不正です: {v}")))?;
        if let ShareTarget::User { id } = target {
            let exists = self
                .resolver
                .user_exists(ctx, id)
                .await
                .map_err(DataError::Internal)?;
            if !exists {
                return Err(DataError::Invalid(format!(
                    "共有先のユーザー '{id}' が見つかりません"
                )));
            }
        }
        Ok(())
    }

    /// 共有・解除の後: SSE の購読へ TTL を待たずに知らせる（世代を進める・PIT-70 ③）。
    async fn after_share_change(&self, ctx: &AuthContext, table_id: Uuid) {
        if let Err(e) = self.notify_authz_changed(ctx, Some(table_id)).await {
            tracing::warn!(error = %e, %table_id, "SSE への共有変更の通知に失敗（購読は TTL で引き直す）");
        }
        // この表を参照する表の購読にも取り直させる（共有先から見たルックアップ・リンク値が変わる）。
        if let Err(e) = self.refresh_referrers(&ctx.tenant_id, table_id).await {
            tracing::warn!(error = %e, %table_id, "共有変更後の参照元の取り直しの発行に失敗");
        }
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
        self.require_record_share_right(
            ctx,
            table_id,
            record_id,
            AuditAction::DataRecordUnshare,
            trace_id,
        )
        .await?;
        let obj = ctx.ns().data_record(&record_id.to_string());
        let subject = target.subject(&ctx.ns());
        let removed = self
            .authz
            .delete_tuple(&subject, role.relation(), &obj)
            .await
            .map_err(|e| DataError::Internal(format!("unshare tuple: {e}")))?;
        // 監査失敗時はタプルを再付与して整合を保つ（撤回が監査に残らない状態を避ける）。
        if let Err(e) = self
            .record_share_audit(
                ctx,
                AuditAction::DataRecordUnshare,
                record_id,
                trace_id,
                json!({ "table_id": table_id, "target": target, "role": role }),
            )
            .await
        {
            if removed {
                if let Err(c) = self
                    .authz
                    .write_tuple(&subject, role.relation(), &obj)
                    .await
                {
                    tracing::error!(error = %c, %record_id, "unshare 監査失敗後のタプル再付与にも失敗");
                }
            }
            return Err(e);
        }
        self.after_share_change(ctx, table_id).await;
        Ok(())
    }

    /// テーブルに「レコード個別共有あり」を立て、依存する材料化列を同一 Tx で降格する。
    async fn mark_record_shared(&self, ctx: &AuthContext, table_id: Uuid) -> Result<(), DataError> {
        let mut table = self.fetch_live(ctx, table_id).await?;
        let mut tx = self
            .pools
            .for_tenant(&ctx.tenant_id)
            .begin()
            .await
            .map_err(map_db)?;
        sqlx::query(
            "UPDATE data_table SET has_record_shares = true \
             WHERE tenant_id = $1 AND id = $2 AND NOT has_record_shares",
        )
        .bind(&ctx.tenant_id)
        .bind(table_id)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        table.has_record_shares = true;
        crate::derived::materialize::demote_dependents(
            &mut tx,
            &ctx.tenant_id,
            table.keys,
            Some((
                table.id,
                &crate::derived::materialize::catalog_entry(&table),
            )),
            "参照先にレコード個別共有が作られた",
        )
        .await?;
        tx.commit().await.map_err(map_db)
    }

    /// 共有権限の検査: テーブル owner または当該レコードの作成者（本人に可視な行のみ）。
    async fn require_record_share_right(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        record_id: Uuid,
        action: AuditAction,
        trace_id: Option<&str>,
    ) -> Result<(), DataError> {
        // 前段: テーブル viewer（第1層）＋行可視性（第2層）。不可視は 404（オラクルなし）。
        let az = self.authz_memo();
        self.require_in(&az, ctx, table_id, Relation::Viewer, action, trace_id)
            .await?;
        let table = self.fetch_live(ctx, table_id).await?;
        let row = self
            .select_visible_by_id(ctx, &az, &table, record_id)
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
        action: AuditAction,
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
