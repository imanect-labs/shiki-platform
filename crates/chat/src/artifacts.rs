//! ツール成果物の保存アダプタ（Task 4.11）。
//!
//! agent-core の [`ArtifactStore`] を `StorageService::write_file_internal`（単一チョークポイント・
//! 認可/監査/content-addressing つき）へ配線する。保存は**発話ユーザーの `AuthContext`** で行い
//! 昇格しない（confused-deputy 回避）。保存先はドライブのルート（org 直下・owner=発話ユーザー）。

use std::sync::Arc;

use agent_core::{ArtifactRef, ArtifactStore, ToolError};
use authz::AuthContext;
use storage::{StorageError, StorageService};

/// `StorageService` 裏の成果物保存（shiki-server 本番配線）。
pub struct StorageArtifactStore {
    storage: Arc<StorageService>,
}

impl StorageArtifactStore {
    pub fn new(storage: Arc<StorageService>) -> Self {
        StorageArtifactStore { storage }
    }
}

#[async_trait::async_trait]
impl ArtifactStore for StorageArtifactStore {
    async fn save(
        &self,
        ctx: &AuthContext,
        name: &str,
        bytes: Vec<u8>,
        content_type: &str,
        trace_id: Option<&str>,
    ) -> Result<ArtifactRef, ToolError> {
        let node = self
            .storage
            .write_file_internal(ctx, None, name, &bytes, content_type, trace_id)
            .await
            .map_err(|e| match e {
                // ゲスト由来の不正名（PIT-23）はモデルが観測して直せる Invalid に写す。
                StorageError::Invalid(msg) => ToolError::Invalid(msg),
                other => ToolError::Unavailable(format!("artifact save: {other}")),
            })?;
        Ok(ArtifactRef {
            node_id: node.id.to_string(),
            name: node.name,
        })
    }
}
