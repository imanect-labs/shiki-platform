//! B1 フロントバンドルの保管（Task 9.11・content-addressed）。
//!
//! バンドルは **単一 self-contained HTML**（CLI が esbuild で固める・Task 9.14）。
//! ユーザーファイルツリー（node/StorageService）とは別枠のアプリ資産で、キーは
//! [`storage::content_address::miniapp_bundle_key`]（sha256 content address・不変）。
//! アップロードは対象 mini_app_code アーティファクトの **owner** のみ。配信側の認可は
//! 第3リスナ（インストール済み突合・app-gateway）が担う。

use std::sync::Arc;

use authz::{AuthContext, AuthzClient, Consistency, Relation};
use storage::audit::{AuditEntry, AuditRecorder, Decision};
use storage::content_address::{miniapp_bundle_key, sha256_hex};
use storage::ObjectStore;
use uuid::Uuid;

use crate::AppPlatformError;

/// 単一 HTML バンドルの上限（5 MiB・self-contained 前提の容量ガード）。
pub const MAX_BUNDLE_BYTES: usize = 5 * 1024 * 1024;

/// バンドル保管の単一チョークポイント。
#[derive(Clone)]
pub struct BundleStore {
    store: Arc<dyn ObjectStore>,
    authz: Arc<dyn AuthzClient>,
    audit: AuditRecorder,
}

impl BundleStore {
    pub fn new(
        store: Arc<dyn ObjectStore>,
        authz: Arc<dyn AuthzClient>,
        audit: AuditRecorder,
    ) -> Self {
        BundleStore {
            store,
            authz,
            audit,
        }
    }

    /// バンドルを保存し content address（sha256 hex）を返す（owner・冪等）。
    pub async fn put(
        &self,
        ctx: &AuthContext,
        artifact_id: Uuid,
        bytes: &[u8],
    ) -> Result<String, AppPlatformError> {
        if bytes.is_empty() {
            return Err(AppPlatformError::Invalid("バンドルが空です".into()));
        }
        if bytes.len() > MAX_BUNDLE_BYTES {
            return Err(AppPlatformError::Invalid(format!(
                "バンドルが上限（{MAX_BUNDLE_BYTES} バイト）を超えています"
            )));
        }
        let obj = ctx.ns().artifact(&artifact_id.to_string());
        let ok = self
            .authz
            .check(
                &ctx.subject(),
                Relation::Owner,
                &obj,
                Consistency::HigherConsistency,
            )
            .await
            .map_err(|e| AppPlatformError::Internal(format!("authz: {e}")))?;
        if !ok {
            return Err(AppPlatformError::Forbidden);
        }
        let sha = sha256_hex(bytes);
        let key = miniapp_bundle_key(&ctx.tenant_id, &sha);
        self.store
            .put_object(&key, bytes.to_vec(), "text/html; charset=utf-8")
            .await
            .map_err(|e| AppPlatformError::Internal(format!("object store: {e}")))?;
        if let Err(e) = self
            .audit
            .record(
                ctx,
                AuditEntry {
                    action: "app.bundle.put",
                    object_type: "miniapp",
                    object_id: &artifact_id.to_string(),
                    decision: Decision::Allow,
                    trace_id: None,
                    metadata: serde_json::json!({ "sha256": sha, "bytes": bytes.len() }),
                },
            )
            .await
        {
            tracing::warn!(error = %e, "バンドル監査の記録に失敗");
        }
        Ok(sha)
    }
}
