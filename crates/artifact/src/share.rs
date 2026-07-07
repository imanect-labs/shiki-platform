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

/// 共有先 id を FGA subject 組み立て前に検証する（storage の共有経路と同一ルール）。
///
/// 前後空白・構造文字（`:` `#` `|` 等）を弾き、不正 id を authz/内部エラーではなく 400 相当の
/// `Invalid` にする（tenant 名前空間化の区切り `|` 混入によるタプル形状崩れも防ぐ）。
fn validate_share_target(target: &ShareTarget) -> Result<(), ArtifactError> {
    let id = match target {
        ShareTarget::User { id } | ShareTarget::Role { id } => id,
    };
    if id != id.trim() {
        return Err(ArtifactError::Invalid(
            "共有先 id の前後に空白は使えません".into(),
        ));
    }
    authz::validate_local_id(id)
        .map_err(|v| ArtifactError::Invalid(format!("共有先 id が不正です: {v}")))
}

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
        validate_share_target(target)?;
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
        validate_share_target(target)?;
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
