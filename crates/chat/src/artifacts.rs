//! ツール成果物の保存アダプタ（Task 4.11）。
//!
//! agent-core の [`ArtifactStore`] を `StorageService::write_file_unique_internal`（単一チョークポイント・
//! 認可/監査/content-addressing つき）へ配線する。保存は**発話ユーザーの `AuthContext`** で行い
//! 昇格しない（confused-deputy 回避）。保存先はドライブのルート（org 直下・owner=発話ユーザー）。
//! 同名が既にあれば ` (2)` `(3)` … を付けて新規作成する（`save_sheet` などの Office 作成と同じ
//! 連番規則・`write_file_unique_internal`）。同じ分析をやり直すたびに保存が「名前が競合」で落ち、
//! モデルが成果物を見失うのを防ぐ。既存ファイルは上書きしない。

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
            .write_file_unique_internal(ctx, None, name, &bytes, content_type, trace_id)
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
