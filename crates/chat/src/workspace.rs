//! ワークスペース保存アダプタ（Task 5.4/5.8・Durable Workspace）。
//!
//! agent-core の [`WorkspaceStore`] を `StorageService`（単一チョークポイント・認可/監査/版管理/書込イベント）
//! へ配線する。ワークスペースは **thread ごとの Drive フォルダ**（`root_folder_id`）で、全操作は
//! **発話ユーザーの `AuthContext`** で実行し昇格しない（confused-deputy 回避）。名前空間はフラット
//! （サブディレクトリ非対応・アルファ）。書込/削除は書込イベント→自動再索引に乗る（PIT-5 と経路分離）。

use std::sync::Arc;

use agent_core::{ToolError, WorkspaceEntry, WorkspaceStore, WorkspaceWrite};
use authz::AuthContext;
use storage::{ChildSort, NodeKind, StorageError, StorageService};
use uuid::Uuid;

/// ワークスペース列挙の 1 回の取得上限（アルファ・フラット namespace のため単一ページ）。
const LIST_LIMIT: usize = 500;

/// `StorageService` 裏のワークスペース CRUD（shiki-server 本番配線）。
pub struct StorageWorkspaceStore {
    storage: Arc<StorageService>,
    /// thread ごとのワークスペースフォルダ（Drive 上の実フォルダ）。
    root_folder_id: Uuid,
}

impl StorageWorkspaceStore {
    pub fn new(storage: Arc<StorageService>, root_folder_id: Uuid) -> Self {
        StorageWorkspaceStore {
            storage,
            root_folder_id,
        }
    }
}

/// StorageError をツール観測用の [`ToolError`] に写す（不正名はモデルが直せる Invalid へ）。
fn map_err(e: StorageError, what: &str) -> ToolError {
    match e {
        StorageError::Invalid(msg) => ToolError::Invalid(msg),
        StorageError::NotFound => ToolError::Invalid(format!("{what}: ファイルが見つかりません")),
        other => ToolError::Unavailable(format!("{what}: {other}")),
    }
}

#[async_trait::async_trait]
impl WorkspaceStore for StorageWorkspaceStore {
    async fn list(
        &self,
        ctx: &AuthContext,
        trace_id: Option<&str>,
    ) -> Result<Vec<WorkspaceEntry>, ToolError> {
        let page = self
            .storage
            .list_children(
                ctx,
                Some(self.root_folder_id),
                ChildSort::default(),
                None,
                LIST_LIMIT,
                trace_id,
            )
            .await
            .map_err(|e| map_err(e, "list"))?;
        Ok(page
            .items
            .into_iter()
            .filter(|n| n.kind == NodeKind::File)
            .map(|n| WorkspaceEntry {
                name: n.name,
                // size は非負に丸めてから u64 化（負値・欠損は 0）。
                size: u64::try_from(n.size_bytes.unwrap_or(0)).unwrap_or(0),
            })
            .collect())
    }

    async fn read(
        &self,
        ctx: &AuthContext,
        name: &str,
        trace_id: Option<&str>,
    ) -> Result<Vec<u8>, ToolError> {
        let node_id = self
            .storage
            .resolve_child_file(ctx, self.root_folder_id, name, trace_id)
            .await
            .map_err(|e| map_err(e, "read"))?
            .ok_or_else(|| ToolError::Invalid(format!("read: '{name}' が見つかりません")))?;
        let (_, bytes) = self
            .storage
            .read_file_internal(ctx, node_id, trace_id)
            .await
            .map_err(|e| map_err(e, "read"))?;
        Ok(bytes)
    }

    async fn write(
        &self,
        ctx: &AuthContext,
        name: &str,
        bytes: Vec<u8>,
        content_type: &str,
        trace_id: Option<&str>,
    ) -> Result<WorkspaceWrite, ToolError> {
        let out = self
            .storage
            .write_file_at(
                ctx,
                self.root_folder_id,
                name,
                &bytes,
                content_type,
                trace_id,
            )
            .await
            .map_err(|e| map_err(e, "write"))?;
        Ok(WorkspaceWrite {
            node_id: out.node_id.to_string(),
            name: name.to_string(),
            version: out.version,
            created: out.created,
        })
    }

    async fn delete(
        &self,
        ctx: &AuthContext,
        name: &str,
        trace_id: Option<&str>,
    ) -> Result<(), ToolError> {
        let node_id = self
            .storage
            .resolve_child_file(ctx, self.root_folder_id, name, trace_id)
            .await
            .map_err(|e| map_err(e, "delete"))?
            .ok_or_else(|| ToolError::Invalid(format!("delete: '{name}' が見つかりません")))?;
        self.storage
            .soft_delete_file(ctx, node_id, trace_id)
            .await
            .map_err(|e| map_err(e, "delete"))?;
        Ok(())
    }
}
