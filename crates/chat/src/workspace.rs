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

/// 1 ページの取得件数（storage 側の 100 件クランプに合わせる）。
const LIST_PAGE: usize = 100;
/// ワークスペース列挙の全体上限（暴走防止・フラット namespace のアルファ既定）。
const MAX_WORKSPACE_FILES: usize = 2000;

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
        // `list_children` は 1 ページ 100 件上限にクランプされるため、`next_cursor` で**全件ページング**する
        // （100 超のワークスペースで file が切れないように）。全体上限 `MAX_WORKSPACE_FILES` で暴走を防ぐ。
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let page = self
                .storage
                .list_children(
                    ctx,
                    Some(self.root_folder_id),
                    ChildSort::default(),
                    cursor.as_deref(),
                    LIST_PAGE,
                    trace_id,
                )
                .await
                .map_err(|e| map_err(e, "list"))?;
            for n in page.items {
                if n.kind == NodeKind::File {
                    out.push(WorkspaceEntry {
                        name: n.name,
                        // size は非負に丸めてから u64 化（負値・欠損は 0）。
                        size: u64::try_from(n.size_bytes.unwrap_or(0)).unwrap_or(0),
                    });
                }
            }
            match page.next_cursor {
                Some(c) if out.len() < MAX_WORKSPACE_FILES => cursor = Some(c),
                _ => break,
            }
        }
        Ok(out)
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
