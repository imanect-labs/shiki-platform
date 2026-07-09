//! skill を artifact（kind=skill）として保存・取得する薄い層（Task 6.7）。
//!
//! バージョン管理・ReBAC 共有・不変バージョンは [`artifact::ArtifactStore`] が担い、
//! 本層は保存前に [`validate_skill_body`] を必ず通す（UiSpecStore / WorkflowStore と同型）。

use std::sync::Arc;

use artifact::{ArtifactError, ArtifactKind, ArtifactStore, NewArtifact};
use authz::AuthContext;
use uuid::Uuid;

use crate::skill::{validate_skill_body, SkillBody};
use crate::store::GuiError;
use crate::validate::GuiValidationError;

/// skill の保存/取得（artifact kind=skill の上）。
#[derive(Clone)]
pub struct SkillStore {
    artifacts: Arc<ArtifactStore>,
}

impl SkillStore {
    pub fn new(artifacts: Arc<ArtifactStore>) -> Self {
        SkillStore { artifacts }
    }

    /// 新しい skill を保存する（検証 → artifact version 1）。
    pub async fn create(
        &self,
        ctx: &AuthContext,
        name: &str,
        raw_body: &serde_json::Value,
        trace_id: Option<&str>,
    ) -> Result<(Uuid, SkillBody), GuiError> {
        let body = validate_skill_body(raw_body).map_err(GuiError::Validation)?;
        let artifact = self
            .artifacts
            .create(
                ctx,
                NewArtifact {
                    kind: ArtifactKind::Skill,
                    name: name.to_string(),
                    body: raw_body.clone(),
                },
                trace_id,
            )
            .await?;
        Ok((artifact.id, body))
    }

    /// 既存 skill に新バージョンを追記する（検証 → 不変追記）。
    pub async fn update(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        raw_body: &serde_json::Value,
        expected_version: Option<i64>,
        trace_id: Option<&str>,
    ) -> Result<(i64, SkillBody), GuiError> {
        let meta = self.artifacts.get(ctx, id, trace_id).await?;
        if meta.kind != ArtifactKind::Skill {
            return Err(GuiError::Validation(vec![GuiValidationError::new(
                "skill.kind_mismatch",
                "このアーティファクトは skill ではありません",
            )]));
        }
        let body = validate_skill_body(raw_body).map_err(GuiError::Validation)?;
        let version = self
            .artifacts
            .append_version(ctx, id, raw_body.clone(), expected_version, trace_id)
            .await?;
        Ok((version.version, body))
    }

    /// 最新バージョンを取得する（viewer・kind 検査つき）。
    pub async fn get_latest(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<(i64, SkillBody, serde_json::Value), GuiError> {
        let meta = self.artifacts.get(ctx, id, trace_id).await?;
        if meta.kind != ArtifactKind::Skill {
            return Err(GuiError::Artifact(ArtifactError::NotFound));
        }
        self.get_version(ctx, id, meta.current_version, trace_id)
            .await
    }

    /// 指定バージョンを不変で取得する（viewer・kind 検査つき）。
    pub async fn get_version(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        version: i64,
        trace_id: Option<&str>,
    ) -> Result<(i64, SkillBody, serde_json::Value), GuiError> {
        let meta = self.artifacts.get(ctx, id, trace_id).await?;
        if meta.kind != ArtifactKind::Skill {
            return Err(GuiError::Artifact(ArtifactError::NotFound));
        }
        let v = self
            .artifacts
            .get_version(ctx, id, version, trace_id)
            .await?;
        // 保存済み body は検証済みだが防御的にパースする（壊れた行を実行面へ流さない）。
        let body = validate_skill_body(&v.body).map_err(GuiError::Validation)?;
        Ok((v.version, body, v.body))
    }
}
