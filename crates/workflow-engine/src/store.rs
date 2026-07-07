//! IR を artifact（kind=workflow）として保存・取得する薄い層（Task 10.1a）。
//!
//! バージョン管理・ReBAC 共有・不変バージョンは [`artifact::ArtifactStore`] が担う。本層は
//! 保存前に [`validate`](crate::validate) を必ず通し、検証済みの IR のみ artifact 本文にする。

use std::sync::Arc;

use artifact::{ArtifactError, ArtifactKind, ArtifactStore, ArtifactVersion, NewArtifact};
use authz::AuthContext;
use uuid::Uuid;

use crate::ir::validate::Catalog;
use crate::{validate, ValidationError, WorkflowIr};

/// ワークフロー保存/取得のエラー。
#[derive(Debug, thiserror::Error)]
pub enum WorkflowStoreError {
    /// 保存時検証エラー（全件）。
    #[error("IR 検証に失敗しました（{} 件）", .0.len())]
    Validation(Vec<ValidationError>),
    #[error(transparent)]
    Artifact(#[from] ArtifactError),
}

impl WorkflowStoreError {
    /// 検証エラーの一覧（あれば）。
    pub fn validation_errors(&self) -> Option<&[ValidationError]> {
        match self {
            WorkflowStoreError::Validation(v) => Some(v),
            WorkflowStoreError::Artifact(_) => None,
        }
    }
}

/// ワークフロー IR の保存/取得（artifact kind=workflow）。
#[derive(Clone)]
pub struct WorkflowStore {
    artifacts: Arc<ArtifactStore>,
}

impl WorkflowStore {
    pub fn new(artifacts: Arc<ArtifactStore>) -> Self {
        WorkflowStore { artifacts }
    }

    /// 新しいワークフローを保存する（V1〜V7 検証 → artifact version 1）。
    pub async fn create(
        &self,
        ctx: &AuthContext,
        ir_json: &serde_json::Value,
        catalog: &Catalog,
        trace_id: Option<&str>,
    ) -> Result<(Uuid, WorkflowIr), WorkflowStoreError> {
        let ir = validate(ir_json, catalog).map_err(WorkflowStoreError::Validation)?;
        let artifact = self
            .artifacts
            .create(
                ctx,
                NewArtifact {
                    kind: ArtifactKind::Workflow,
                    name: ir.name.clone(),
                    body: ir_json.clone(),
                },
                trace_id,
            )
            .await?;
        Ok((artifact.id, ir))
    }

    /// 既存ワークフローに新バージョンを追記する（V1〜V7 検証 → 不変追記）。
    pub async fn update(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        ir_json: &serde_json::Value,
        catalog: &Catalog,
        expected_version: Option<i64>,
        trace_id: Option<&str>,
    ) -> Result<(i64, WorkflowIr), WorkflowStoreError> {
        let ir = validate(ir_json, catalog).map_err(WorkflowStoreError::Validation)?;
        // 対象 artifact が workflow 種であり、名前が IR と一致することを確認する。
        // 汎用 artifact API 経由で作られた別種（prompt/mini_app 等）を workflow エンドポイントで
        // 上書きして種不変条件を壊すこと・参照名の齟齬（workflow.start の名前解決が狂う）を防ぐ。
        let meta = self.artifacts.get(ctx, id, trace_id).await?;
        if meta.kind != ArtifactKind::Workflow {
            return Err(WorkflowStoreError::Validation(vec![ValidationError::new(
                "ir.kind_mismatch",
                "このアーティファクトは workflow ではありません".to_string(),
            )]));
        }
        if meta.name != ir.name {
            return Err(WorkflowStoreError::Validation(vec![ValidationError::new(
                "ir.name_mismatch",
                format!(
                    "IR の name（{}）が既存アーティファクト名（{}）と一致しません",
                    ir.name, meta.name
                ),
            )]));
        }
        let version = self
            .artifacts
            .append_version(ctx, id, ir_json.clone(), expected_version, trace_id)
            .await?;
        Ok((version.version, ir))
    }

    /// 最新バージョンの IR を取得する。
    pub async fn get_latest(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<(i64, WorkflowIr), WorkflowStoreError> {
        let meta = self.artifacts.get(ctx, id, trace_id).await?;
        let version = self
            .artifacts
            .get_version(ctx, id, meta.current_version, trace_id)
            .await?;
        parse_version(version)
    }

    /// 指定バージョンの IR を不変で取得する（旧版が変わらないこと）。
    pub async fn get_version(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        version: i64,
        trace_id: Option<&str>,
    ) -> Result<(i64, WorkflowIr), WorkflowStoreError> {
        let v = self
            .artifacts
            .get_version(ctx, id, version, trace_id)
            .await?;
        parse_version(v)
    }
}

/// artifact バージョン本文を IR へパースする（保存済みなので構造は妥当だが防御的に扱う）。
#[allow(clippy::needless_pass_by_value)]
fn parse_version(v: ArtifactVersion) -> Result<(i64, WorkflowIr), WorkflowStoreError> {
    match WorkflowIr::from_json(&v.body) {
        Ok(ir) => Ok((v.version, ir)),
        Err(e) => Err(WorkflowStoreError::Validation(vec![ValidationError::new(
            "ir.schema_violation",
            format!("保存済み IR のパースに失敗しました: {e}"),
        )])),
    }
}
