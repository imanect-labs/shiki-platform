//! 図のボタンのワークフローを名前で解決する（#604）。
//!
//! ボタンは `-> workflow 名前` と名前だけを書く。回答が確定したときに**回答した主体**の
//! viewer 権限で引き、current の版に固定する（アンビエント権限なし・存在秘匿）。押した人の
//! 権限は起動時に workflow-engine が別に確かめる（ここでは昇格しない）。

use std::sync::Arc;

use artifact::{ArtifactError, ArtifactKind, ArtifactStore};
use authz::AuthContext;

use crate::action::WorkflowPin;

/// ワークフロー名の解決（会話の図のボタンの束縛に使う）。
#[derive(Clone)]
pub struct WorkflowResolver {
    artifacts: Arc<ArtifactStore>,
}

impl WorkflowResolver {
    pub fn new(artifacts: Arc<ArtifactStore>) -> Self {
        WorkflowResolver { artifacts }
    }

    /// ワークフローを名前で解決して current の版に固定する。
    ///
    /// 不在・権限なし・他種のアーティファクトは区別せず `None`（存在秘匿）。
    pub async fn resolve_workflow_name(
        &self,
        ctx: &AuthContext,
        name: &str,
        trace_id: Option<&str>,
    ) -> Option<WorkflowPin> {
        let meta = match self
            .artifacts
            .get_by_name(ctx, ArtifactKind::Workflow, name.trim(), trace_id)
            .await
        {
            Ok(meta) => meta,
            Err(ArtifactError::NotFound | ArtifactError::Forbidden) => return None,
            Err(e) => {
                tracing::warn!(error = %e, "会話の図のワークフローを解決できない");
                return None;
            }
        };
        if meta.kind != ArtifactKind::Workflow {
            return None;
        }
        Some(WorkflowPin {
            name: Some(meta.name),
            artifact_id: Some(meta.id),
            version: Some(meta.current_version),
        })
    }
}
