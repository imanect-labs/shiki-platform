//! 会話の添付ファイル取得アダプタ（issue #379）。
//!
//! agent-core の [`AttachmentStore`] を `StorageService`（単一チョークポイント・認可/監査）へ
//! 配線する。読み取りは**発話ユーザーの `AuthContext`** で行い昇格しない（confused-deputy 回避）。
//! `code_interpreter` はこれを使って添付を guest `/workspace/<name>` へ置く。

use std::sync::Arc;

use agent_core::{AttachmentStore, ToolError};
use authz::AuthContext;
use storage::{StorageError, StorageService};
use uuid::Uuid;

/// `StorageService` 裏の添付取得（shiki-server 本番配線）。
pub struct StorageAttachmentStore {
    storage: Arc<StorageService>,
}

impl StorageAttachmentStore {
    pub fn new(storage: Arc<StorageService>) -> Self {
        StorageAttachmentStore { storage }
    }
}

#[async_trait::async_trait]
impl AttachmentStore for StorageAttachmentStore {
    async fn read(
        &self,
        ctx: &AuthContext,
        node_id: &str,
        max_bytes: u64,
        trace_id: Option<&str>,
    ) -> Result<Vec<u8>, ToolError> {
        let id = Uuid::parse_str(node_id)
            .map_err(|_| ToolError::Invalid("node_id が UUID ではありません".into()))?;
        // サイズはメタデータ（DB 行）で先に見る。blob を読んでから捨てるとサンドボックスへ
        // 運ばないファイルのためにメモリと帯域を使うことになる。
        let node = self
            .storage
            .get_metadata(ctx, id, trace_id)
            .await
            .map_err(map_err)?;
        let size = u64::try_from(node.size_bytes.unwrap_or(0)).unwrap_or(u64::MAX);
        if size > max_bytes {
            return Err(ToolError::Invalid(format!(
                "サイズ上限（{max_bytes} バイト）を超えています"
            )));
        }
        let (_, bytes) = self
            .storage
            .read_file_internal(ctx, id, trace_id)
            .await
            .map_err(map_err)?;
        Ok(bytes)
    }
}

/// StorageError をツール観測用の [`ToolError`] へ写す。
///
/// 権限なし/未検出は**同一メッセージへ畳む**（存在秘匿・#326。「権限が無い」と返すと
/// 会話越しにノードの実在が漏れる）。
fn map_err(e: StorageError) -> ToolError {
    match e {
        StorageError::NotFound | StorageError::Forbidden => {
            ToolError::Invalid("添付にアクセスできません（存在しないか、権限がありません）".into())
        }
        StorageError::Invalid(msg) => ToolError::Invalid(msg),
        other => ToolError::Unavailable(format!("attachment read: {other}")),
    }
}
