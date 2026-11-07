//! ワークフローを名前で探す（AI のツール `list_workflows`・GAP-31）。
//!
//! 見えるかは一覧（`GET /workflows`）と同じ（自分が持ち主のもの ∪ viewer として共有されたもの・二段の認可は
//! [`artifact::ArtifactStore::search_visible`]）。要約は一覧と同じ射影（[`WorkflowSummaryStore::list`]）に、
//! 実行の入力の項目（[`WorkflowSummaryStore::inputs`]）と、本人が編集できるか（新しい版を保存できるか）を足す。
//! IR の本文は運ばない（読むのは `read_workflow`・`artifact.read` の監査が残る）。

use std::collections::HashMap;

use artifact::{ArtifactError, ArtifactKind, SearchRequest};
use authz::AuthContext;
use uuid::Uuid;

use crate::store::{WorkflowStore, WorkflowStoreError};
use crate::summary::{WorkflowInputs, WorkflowSummary, WorkflowSummaryStore};

/// 探して見つかったワークフロー 1 件。
#[derive(Debug, Clone)]
pub struct FoundWorkflow {
    pub summary: WorkflowSummary,
    /// 本人が持ち主か（偽なら共有されたもの）。
    pub owned: bool,
    /// 本人が編集できるか（新しい版を保存できるか・表示の出し分けだけ。保存の口が改めて確かめる）。
    pub editable: bool,
    /// 実行の入力の項目（`x-order` の順に先に）。
    pub input_keys: Vec<String>,
    /// 必須の入力の項目。
    pub required_inputs: Vec<String>,
}

/// [`WorkflowStore::find`] の結果。
#[derive(Debug, Clone)]
pub struct FoundWorkflows {
    /// 更新の新しい順。
    pub items: Vec<FoundWorkflow>,
    /// ほかにも当たるものがある（かもしれない）。
    pub more: bool,
}

/// [`WorkflowStore::find`] の頼み。
#[derive(Debug, Clone, Copy)]
pub struct FindWorkflows<'a> {
    /// 名前・表示名・説明に含む語（省略は絞らない）。
    pub query: Option<&'a str>,
    /// 共有されたものも含めるか。
    pub include_shared: bool,
    /// 件数（1〜100）。
    pub limit: i64,
}

impl WorkflowStore {
    /// 本人に見えるワークフローを名前で探す（新しい順・要約と入力の項目と編集できるか）。
    pub async fn find(
        &self,
        ctx: &AuthContext,
        summaries: &WorkflowSummaryStore,
        req: FindWorkflows<'_>,
    ) -> Result<FoundWorkflows, WorkflowStoreError> {
        let page = self
            .artifacts
            .search_visible(
                ctx,
                SearchRequest {
                    kind: ArtifactKind::Workflow,
                    query: req.query,
                    include_shared: req.include_shared,
                    limit: req.limit,
                },
            )
            .await?;
        let editable = self.artifacts.editable_flags(ctx, &page.items).await?;
        let flags: HashMap<Uuid, (bool, bool)> = page
            .items
            .iter()
            .zip(editable)
            .map(|(a, e)| (a.id, (a.owner == ctx.principal().id, e)))
            .collect();
        let ids: Vec<Uuid> = page.items.iter().map(|a| a.id).collect();
        let db = |e: sqlx::Error| ArtifactError::Internal(format!("ワークフローの要約: {e}"));
        let rows = summaries.list(ctx.tenant_id(), &ids).await.map_err(db)?;
        let mut inputs: HashMap<Uuid, WorkflowInputs> = summaries
            .inputs(ctx.tenant_id(), &ids)
            .await
            .map_err(db)?
            .into_iter()
            .map(|i| (i.id, i))
            .collect();
        let items = rows
            .into_iter()
            .filter_map(|summary| {
                let (owned, editable) = *flags.get(&summary.id)?;
                let inputs = inputs.remove(&summary.id);
                Some(FoundWorkflow {
                    owned,
                    editable,
                    input_keys: inputs.as_ref().map(|i| i.keys.clone()).unwrap_or_default(),
                    required_inputs: inputs.map(|i| i.required).unwrap_or_default(),
                    summary,
                })
            })
            .collect();
        Ok(FoundWorkflows {
            items,
            more: page.more,
        })
    }

    /// 名前（IR の `name`）か id（UUID）でワークフローを引く（本人が viewer のものだけ）。
    ///
    /// 見えないものと無いものは同じ `NotFound` にする（名前は当てられるので、権限の無さを区別すると、名前の
    /// ワークフローがあることを明かす）。見えないものは viewer の確かめが Deny の監査に残す。
    pub async fn resolve_ref(
        &self,
        ctx: &AuthContext,
        workflow: &str,
        trace_id: Option<&str>,
    ) -> Result<artifact::Artifact, WorkflowStoreError> {
        let workflow = workflow.trim();
        let got = match Uuid::parse_str(workflow) {
            Ok(id) => self.artifacts.get(ctx, id, trace_id).await,
            Err(_) => {
                self.artifacts
                    .get_by_name(ctx, ArtifactKind::Workflow, workflow, trace_id)
                    .await
            }
        };
        match got {
            Ok(meta) if meta.kind == ArtifactKind::Workflow => Ok(meta),
            Ok(_) | Err(ArtifactError::NotFound | ArtifactError::Forbidden) => {
                Err(ArtifactError::NotFound.into())
            }
            Err(e) => Err(e.into()),
        }
    }
}
