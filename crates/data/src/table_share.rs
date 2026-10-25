//! テーブルの共有（#864・テーブル ReBAC の viewer / editor をユーザー・ロールへ付け外しする）。
//!
//! 共有 = OpenFGA の `data_table` タプルの付与・削除だけ（DB 行は変えない）。保存ビュー（artifact）の
//! 共有と同じ枠で、
//!
//! - 共有・解除・共有先の一覧ができるのは**テーブルの owner**（`Relation::Owner`）だけ。テーブルが
//!   見えない主体には 404（存在しない・削除済みと区別しない）、見えるが owner でない主体には 403。
//! - 付与できる役割は viewer / editor だけ（owner の横展開はしない）。
//! - 共有先は id の形と**同一テナントのディレクトリに居ること**を検査する（レコードの個別共有と同じ）。
//!   解除は形だけ検査する（残った不正なタプルも消せるように）。
//! - 監査に失敗したらタプルを補償して戻す（監査に残らない共有・解除を作らない）。
//! - 付け外しの後は [`DataStore::after_table_rebac_changed`] を呼ぶ: 配信の認可の世代を進め（解除した相手の
//!   購読は次のバッチで `stream.reset{authz_changed}` で閉じる・§9.4）、このテーブルとこれを参照先にする
//!   表の材料化の前提（viewer 集合の包含・§7.4・PIT-71）を検査し直す。REST の読取は毎回 OpenFGA を
//!   引くので解除は即時に効く（PIT-18）。

use authz::{AuthContext, Relation};
use serde::{Deserialize, Serialize};
use serde_json::json;
use storage::audit::AuditAction;
use storage::ShareTarget;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::record_share::check_share_target_id;
use crate::store::DataStore;
use crate::DataError;

/// テーブル共有の役割（共有語彙は viewer / editor のみ・owner は付け外ししない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TableShareRole {
    /// 読める（行述語・フィールドマスクは閲覧者本人の権限で効く）。
    Viewer,
    /// 読み書きできる（行述語の書込条件は各行で効く）。
    Editor,
}

impl TableShareRole {
    fn relation(self) -> Relation {
        match self {
            TableShareRole::Viewer => Relation::Viewer,
            TableShareRole::Editor => Relation::Editor,
        }
    }

    fn from_relation(relation: Relation) -> Option<Self> {
        match relation {
            Relation::Viewer => Some(TableShareRole::Viewer),
            Relation::Editor => Some(TableShareRole::Editor),
            _ => None,
        }
    }
}

impl DataStore {
    /// テーブルを共有する（owner・viewer / editor・冪等）。
    pub async fn share_table(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        target: &ShareTarget,
        role: TableShareRole,
        trace_id: Option<&str>,
    ) -> Result<(), DataError> {
        self.require_table_owner(ctx, table_id, AuditAction::DataTableShare, trace_id)
            .await?;
        self.validate_share_target(ctx, target).await?;
        let obj = ctx.ns().data_table(&table_id.to_string());
        let subject = target.subject(&ctx.ns());
        let wrote = self
            .authz
            .write_tuple(&subject, role.relation(), &obj)
            .await
            .map_err(|e| DataError::Internal(format!("share table tuple: {e}")))?;
        // 監査失敗時はタプルを補償（監査なしの共有を残さない）。撤回は実際に付与した場合のみ
        // （冪等 write の false は既存タプル＝撤回しない）。
        if let Err(e) = self
            .record_audit(
                ctx,
                AuditAction::DataTableShare,
                &table_id.to_string(),
                trace_id,
                json!({ "target": target, "role": role }),
            )
            .await
        {
            if wrote {
                if let Err(c) = self
                    .authz
                    .delete_tuple(&subject, role.relation(), &obj)
                    .await
                {
                    tracing::error!(error = %c, %table_id, "テーブル共有の監査失敗後のタプル補償削除にも失敗");
                }
            }
            return Err(e);
        }
        self.after_table_rebac_changed(ctx, table_id).await;
        Ok(())
    }

    /// テーブルの共有を解除する（owner・冪等・即時反映）。
    pub async fn unshare_table(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        target: &ShareTarget,
        role: TableShareRole,
        trace_id: Option<&str>,
    ) -> Result<(), DataError> {
        self.require_table_owner(ctx, table_id, AuditAction::DataTableUnshare, trace_id)
            .await?;
        check_share_target_id(target)?;
        let obj = ctx.ns().data_table(&table_id.to_string());
        let subject = target.subject(&ctx.ns());
        let removed = self
            .authz
            .delete_tuple(&subject, role.relation(), &obj)
            .await
            .map_err(|e| DataError::Internal(format!("unshare table tuple: {e}")))?;
        // 監査失敗時はタプルを再付与して整合を保つ（撤回が監査に残らない状態を避ける）。
        if let Err(e) = self
            .record_audit(
                ctx,
                AuditAction::DataTableUnshare,
                &table_id.to_string(),
                trace_id,
                json!({ "target": target, "role": role }),
            )
            .await
        {
            if removed {
                if let Err(c) = self
                    .authz
                    .write_tuple(&subject, role.relation(), &obj)
                    .await
                {
                    tracing::error!(error = %c, %table_id, "テーブル共有解除の監査失敗後のタプル再付与にも失敗");
                }
            }
            return Err(e);
        }
        self.after_table_rebac_changed(ctx, table_id).await;
        Ok(())
    }

    /// テーブルの共有先の一覧（owner・ユーザー / ロールへの直接の viewer / editor タプルのみ）。
    ///
    /// owner タプル・ワークフローやミニアプリの主体への委譲は共有ではないので出さない。
    pub async fn list_table_shares(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<Vec<(ShareTarget, TableShareRole)>, DataError> {
        self.require_table_owner(ctx, table_id, AuditAction::DataTableSharesList, trace_id)
            .await?;
        let obj = ctx.ns().data_table(&table_id.to_string());
        let tuples = self
            .authz
            .read_tuples(&obj, None)
            .await
            .map_err(|e| DataError::Internal(format!("read table tuples: {e}")))?;
        let ns = ctx.ns();
        Ok(tuples
            .into_iter()
            .filter_map(|t| {
                let role = Relation::parse(&t.relation).and_then(TableShareRole::from_relation)?;
                let target = ShareTarget::parse_subject(&ns, &t.user)?;
                Some((target, role))
            })
            .collect())
    }

    /// 共有の操作の権限: テーブルの owner（不可視は 404・owner でなければ 403・不足は監査 deny）。
    /// 削除済みのテーブルは 404。
    async fn require_table_owner(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        action: AuditAction,
        trace_id: Option<&str>,
    ) -> Result<(), DataError> {
        self.require(ctx, table_id, Relation::Owner, action, trace_id)
            .await?;
        self.fetch_live(ctx, table_id).await.map(|_| ())
    }
}
