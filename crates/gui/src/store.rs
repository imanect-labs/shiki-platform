//! UI スペックを artifact（kind=ui_spec）として保存・取得する薄い層（Task 6.3 保存路）。
//!
//! バージョン管理・ReBAC 共有・不変バージョンは [`artifact::ArtifactStore`] が担う。本層は
//! 保存前に [`SpecValidator`] を必ず通し、**検証・解決済みのスペックのみ**を本文にする
//! （workflow-engine の `WorkflowStore` と同型）。

use std::sync::Arc;

use artifact::{ArtifactError, ArtifactKind, ArtifactStore, NewArtifact};
use authz::AuthContext;
use uuid::Uuid;

use crate::validate::GuiValidationError;
use crate::validator::{ResolvedSpec, SpecValidator};

/// UI スペック保存/取得のエラー。
#[derive(Debug, thiserror::Error)]
pub enum GuiError {
    /// 検証エラー（全件）。
    #[error("UI スペック検証に失敗しました（{} 件）", .0.len())]
    Validation(Vec<GuiValidationError>),
    #[error(transparent)]
    Artifact(#[from] ArtifactError),
}

impl GuiError {
    /// 検証エラーの一覧（あれば）。
    pub fn validation_errors(&self) -> Option<&[GuiValidationError]> {
        match self {
            GuiError::Validation(v) => Some(v),
            GuiError::Artifact(_) => None,
        }
    }
}

/// UI スペックの保存/取得（artifact kind=ui_spec の上）。
#[derive(Clone)]
pub struct UiSpecStore {
    artifacts: Arc<ArtifactStore>,
    validator: Arc<SpecValidator>,
}

impl UiSpecStore {
    pub fn new(artifacts: Arc<ArtifactStore>, validator: Arc<SpecValidator>) -> Self {
        UiSpecStore {
            artifacts,
            validator,
        }
    }

    /// 新しい UI スペックを保存する（検証・解決 → artifact version 1）。
    pub async fn create(
        &self,
        ctx: &AuthContext,
        name: &str,
        raw_spec: &serde_json::Value,
        trace_id: Option<&str>,
    ) -> Result<(Uuid, ResolvedSpec), GuiError> {
        let resolved = self
            .validator
            .validate(ctx, raw_spec, "save", trace_id)
            .await
            .map_err(GuiError::Validation)?;
        let artifact = self
            .artifacts
            .create(
                ctx,
                NewArtifact {
                    kind: ArtifactKind::UiSpec,
                    name: name.to_string(),
                    body: resolved.json.clone(),
                },
                trace_id,
            )
            .await?;
        Ok((artifact.id, resolved))
    }

    /// 既存 UI スペックに新バージョンを追記する（検証・解決 → 不変追記）。
    pub async fn update(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        raw_spec: &serde_json::Value,
        expected_version: Option<i64>,
        trace_id: Option<&str>,
    ) -> Result<(i64, ResolvedSpec), GuiError> {
        // 対象が ui_spec 種であることを確認する（他種 artifact をこのエンドポイントで
        // 上書きして kind 不変条件を壊さない・WorkflowStore と同じ防御）。
        let meta = self.artifacts.get(ctx, id, trace_id).await?;
        if meta.kind != ArtifactKind::UiSpec {
            return Err(GuiError::Validation(vec![GuiValidationError::new(
                "gui.kind_mismatch",
                "このアーティファクトは ui_spec ではありません",
            )]));
        }
        let resolved = self
            .validator
            .validate(ctx, raw_spec, "save", trace_id)
            .await
            .map_err(GuiError::Validation)?;
        let version = self
            .artifacts
            .append_version(ctx, id, resolved.json.clone(), expected_version, trace_id)
            .await?;
        Ok((version.version, resolved))
    }

    /// 最新バージョンの本文を取得する（viewer・kind 検査つき）。
    pub async fn get_latest(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<(i64, serde_json::Value), GuiError> {
        let meta = self.artifacts.get(ctx, id, trace_id).await?;
        if meta.kind != ArtifactKind::UiSpec {
            return Err(GuiError::Artifact(ArtifactError::NotFound));
        }
        let v = self
            .artifacts
            .get_version(ctx, id, meta.current_version, trace_id)
            .await?;
        Ok((v.version, v.body))
    }

    /// 指定バージョンの本文を不変で取得する（viewer・kind 検査つき）。
    pub async fn get_version(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        version: i64,
        trace_id: Option<&str>,
    ) -> Result<(i64, serde_json::Value), GuiError> {
        let meta = self.artifacts.get(ctx, id, trace_id).await?;
        if meta.kind != ArtifactKind::UiSpec {
            return Err(GuiError::Artifact(ArtifactError::NotFound));
        }
        let v = self
            .artifacts
            .get_version(ctx, id, version, trace_id)
            .await?;
        Ok((v.version, v.body))
    }
}
