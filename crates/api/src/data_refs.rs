//! data サービスの参照整合検証アダプタ（Task 9.2）。
//!
//! user/role は directory（テナントスコープの射影）、file は StorageService
//! （存在＋**呼出ユーザーの可読**を単一チョークポイントの authz 込みで検証）。
//! data crate はストレージ実体へ依存せず、このアダプタ注入で検証を受ける。

use std::sync::Arc;

use async_trait::async_trait;
use authz::AuthContext;
use data::RefResolver;
use storage::{DirectoryStore, StorageError, StorageService};
use uuid::Uuid;

pub struct ApiRefResolver {
    pub directory: Arc<DirectoryStore>,
    pub storage: Arc<StorageService>,
}

#[async_trait]
impl RefResolver for ApiRefResolver {
    async fn user_exists(&self, ctx: &AuthContext, user_id: &str) -> Result<bool, String> {
        self.directory
            .user_exists(ctx, user_id)
            .await
            .map_err(|e| format!("directory: {e}"))
    }

    async fn role_exists(&self, ctx: &AuthContext, role_id: &str) -> Result<bool, String> {
        self.directory
            .role_exists(ctx, role_id)
            .await
            .map_err(|e| format!("directory: {e}"))
    }

    async fn file_readable(&self, ctx: &AuthContext, file_id: Uuid) -> Result<bool, String> {
        // 不存在と未認可は同じ false（file_ref 検証で存在オラクルを作らない）。
        match self.storage.get_metadata(ctx, file_id, None).await {
            Ok(_) => Ok(true),
            Err(StorageError::NotFound | StorageError::Forbidden) => Ok(false),
            Err(e) => Err(format!("storage: {e}")),
        }
    }
}
