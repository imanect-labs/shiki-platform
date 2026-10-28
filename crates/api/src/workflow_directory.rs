//! 本人に見えるワークフローの検索源（chat の `search_workflows` へ注入する）。
//!
//! 認可は**ワークフロー一覧 API（`GET /workflows`）と同じ経路**: artifact の所有一覧
//! （`list_mine`）∪ 共有一覧（`list_shared_with_me` = FGA ListObjects の viewer 実効集合）で
//! 認可済み id 集合を確定し、その集合だけを `WorkflowSummaryStore::search` が SQL で絞る
//! （PIT-13: 可読集合を先に確定して DB 側で `IN` 絞り込み）。発話ユーザーの `AuthContext` の
//! まま呼び、昇格しない。tenant は artifact 層と射影 SQL の両方で絞る（二重防御）。

use std::sync::Arc;

use artifact::{ArtifactError, ArtifactKind, ArtifactStore};
use authz::AuthContext;
use uuid::Uuid;
use workflow_engine::{WorkflowSearchHit, WorkflowSummaryStore};

/// artifact 一覧の 1 ページ（artifact 層の上限と同じ）。
const PAGE: i64 = 100;
/// 所有分を辿るページ数の上限（1000 件）。これを超える古い所有分は検索に載らない。
const MAX_OWNED_PAGES: usize = 10;

/// 本人に見えるワークフローの id 集合（所有∪共有・重複なし）と、そのうち所有分。
///
/// 所有分は keyset で辿り切る（上限 [`MAX_OWNED_PAGES`]）。共有分は artifact 層の上限
/// （更新の新しい 100 件）までで、一覧 API の初回ページと同じ範囲になる。
pub async fn visible_workflow_ids(
    artifacts: &ArtifactStore,
    ctx: &AuthContext,
) -> Result<(Vec<Uuid>, Vec<Uuid>), ArtifactError> {
    let mut owned: Vec<Uuid> = Vec::new();
    let mut before = None;
    for _ in 0..MAX_OWNED_PAGES {
        let page = artifacts
            .list_mine(ctx, Some(ArtifactKind::Workflow), before, PAGE)
            .await?;
        owned.extend(page.iter().map(|a| a.id));
        if (page.len() as i64) < PAGE {
            break;
        }
        before = page.last().map(|a| (a.updated_at, a.id));
    }
    let shared = artifacts
        .list_shared_with_me(ctx, Some(ArtifactKind::Workflow), PAGE)
        .await?;
    let mut ids = owned.clone();
    ids.extend(shared.iter().map(|a| a.id));
    ids.sort_unstable();
    ids.dedup();
    owned.sort_unstable();
    owned.dedup();
    Ok((ids, owned))
}

/// chat ワーカー（`search_workflows`）へ注入する検索源。
pub struct ApiWorkflowDirectory {
    artifacts: Arc<ArtifactStore>,
    summaries: WorkflowSummaryStore,
}

impl ApiWorkflowDirectory {
    #[must_use]
    pub fn new(artifacts: Arc<ArtifactStore>, summaries: WorkflowSummaryStore) -> Self {
        ApiWorkflowDirectory {
            artifacts,
            summaries,
        }
    }
}

#[async_trait::async_trait]
impl chat::WorkflowDirectory for ApiWorkflowDirectory {
    async fn search(
        &self,
        ctx: &AuthContext,
        query: &str,
        interactive_only: bool,
        limit: usize,
    ) -> Result<Vec<WorkflowSearchHit>, String> {
        let (ids, owned) = visible_workflow_ids(&self.artifacts, ctx)
            .await
            .map_err(|e| format!("ワークフロー一覧: {e}"))?;
        self.summaries
            .search(
                &ctx.tenant_id,
                &ids,
                &owned,
                query,
                interactive_only,
                limit as i64,
            )
            .await
            .map_err(|e| format!("ワークフロー検索: {e}"))
    }
}
