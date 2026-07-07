//! ReBAC 共有（owner 権限・viewer/editor・Phase 3.7 と同じ枠）。
//!
//! 共有 = OpenFGA タプルの付与/削除のみ（DB 行は変更しない）。監査失敗時は補償で
//! タプルを戻す（thread 共有・#37 と同一パターン）。

use authz::{AuthContext, Relation};
use serde_json::json;
use storage::model::ShareTarget;
use uuid::Uuid;

use crate::model::ArtifactRole;
use crate::store::ArtifactStore;
use crate::ArtifactError;

impl ArtifactStore {
    /// アーティファクトを共有する（owner 権限・viewer/editor）。
    pub async fn share(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        target: &ShareTarget,
        role: ArtifactRole,
        trace_id: Option<&str>,
    ) -> Result<(), ArtifactError> {
        let obj = self
            .require(ctx, id, Relation::Owner, "artifact.share", trace_id)
            .await?;
        let granted = self
            .authz
            .write_tuple(&target.subject(&ctx.ns()), role.relation(), &obj)
            .await
            .map_err(|e| ArtifactError::Internal(e.to_string()))?;
        if let Err(e) = self
            .record_audit(
                ctx,
                "artifact.share",
                &id.to_string(),
                trace_id,
                json!({ "target": target, "role": role }),
            )
            .await
        {
            if granted {
                let _ = self
                    .authz
                    .delete_tuple(&target.subject(&ctx.ns()), role.relation(), &obj)
                    .await;
            }
            return Err(e);
        }
        Ok(())
    }

    /// 共有を解除する（owner 権限・冪等・即時反映）。
    pub async fn unshare(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        target: &ShareTarget,
        role: ArtifactRole,
        trace_id: Option<&str>,
    ) -> Result<(), ArtifactError> {
        let obj = self
            .require(ctx, id, Relation::Owner, "artifact.unshare", trace_id)
            .await?;
        let revoked = self
            .authz
            .delete_tuple(&target.subject(&ctx.ns()), role.relation(), &obj)
            .await
            .map_err(|e| ArtifactError::Internal(e.to_string()))?;
        if let Err(e) = self
            .record_audit(
                ctx,
                "artifact.unshare",
                &id.to_string(),
                trace_id,
                json!({ "target": target, "role": role }),
            )
            .await
        {
            if revoked {
                let _ = self
                    .authz
                    .write_tuple(&target.subject(&ctx.ns()), role.relation(), &obj)
                    .await;
            }
            return Err(e);
        }
        Ok(())
    }

    /// 共有相手一覧（owner 権限・直接タプルのみ）。
    pub async fn list_shares(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<Vec<(ShareTarget, ArtifactRole)>, ArtifactError> {
        let obj = self
            .require(ctx, id, Relation::Owner, "artifact.shares.list", trace_id)
            .await?;
        let tuples = self
            .authz
            .read_tuples(&obj, None)
            .await
            .map_err(|e| ArtifactError::Internal(e.to_string()))?;
        let mut out = Vec::new();
        for t in tuples {
            let Some(role) = Relation::parse(&t.relation).and_then(ArtifactRole::from_relation)
            else {
                continue;
            };
            let Some(target) = ShareTarget::parse_subject(&ctx.ns(), &t.user) else {
                continue;
            };
            out.push((target, role));
        }
        Ok(out)
    }
}
