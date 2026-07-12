//! インストールのライフサイクル操作（Task 9.13b・`install.rs` から分割・500 行規約）。
//!
//! アンインストール・オフライン import・補償/監査/outbox ヘルパ。本体（同意インストール）は
//! [`crate::install`]。

use storage::audit::{AuditEntry, Decision};
use storage::event::{emit_on, WriteEvent, WriteOp};
use uuid::Uuid;

use authz::{AuthContext, Relation};

use crate::install::map_gateway;
use crate::sign::verify_manifest_signature;
use crate::{AppPlatformError, InstallService, MiniAppManifest};

impl InstallService {
    /// アンインストール: 失効（即時 403）→ テーブル archive ＋ FGA tuple 撤去 → client 無効化。
    pub async fn uninstall(
        &self,
        ctx: &AuthContext,
        app_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<(), AppPlatformError> {
        self.require_artifact_owner(ctx, app_id).await?;
        // 失効を先に（gateway は次のリクエストから 403＝トークン有効期限内でも遮断）。
        let installation = self
            .installations
            .resolve_active_by_app(&ctx.tenant_id, app_id)
            .await
            .map_err(map_gateway)?;
        self.installations
            .revoke(ctx, app_id)
            .await
            .map_err(map_gateway)?;

        // 所有テーブルを archive（soft delete）し tuple を撤去する。
        let tables = self.data.table_ids_owned_by_app(ctx, app_id).await?;
        for id in &tables {
            if let Err(e) = self.data.delete_table(ctx, *id, trace_id).await {
                tracing::warn!(error = %e, table_id = %id, "アンインストール時のテーブル archive に失敗");
            }
            let obj = ctx.ns().data_table(&id.to_string());
            if let Err(e) = self.authz.delete_object_tuples(&obj).await {
                tracing::warn!(error = %e, table_id = %id, "アンインストール時の tuple 撤去に失敗");
            }
        }
        if let Some(inst) = &installation {
            self.disable_clients_best_effort(
                inst.client_id_b1.as_deref(),
                inst.client_id_b2.as_deref(),
            )
            .await;
        }
        self.record_audit(ctx, app_id, "app.uninstall", Decision::Allow, trace_id)
            .await;
        self.emit_app_event(
            ctx,
            app_id,
            "app.uninstalled",
            serde_json::json!({}),
            trace_id,
        )
        .await;
        Ok(())
    }

    /// オフライン（エアギャップ）import: 署名検証 → artifact 作成 → 不変 publish。
    ///
    /// 署名は**常に必須**（ネット非依存の信頼根＝信頼鍵台帳）。検証に成功した場合のみ
    /// 呼出ユーザーを owner として artifact を作る。
    pub async fn import_signed(
        &self,
        ctx: &AuthContext,
        manifest: MiniAppManifest,
        signature: &[u8],
        key_id: &str,
        trace_id: Option<&str>,
    ) -> Result<crate::RegistryEntry, AppPlatformError> {
        let key = self
            .keys
            .find_active(ctx, key_id)
            .await?
            .ok_or(AppPlatformError::Forbidden)?;
        verify_manifest_signature(&manifest, signature, &key)?;
        let id = self.code.create(ctx, &manifest, trace_id).await?;
        let entry = self
            .code
            .publish(ctx, id, None, Some(signature), trace_id)
            .await?;
        self.record_audit(ctx, id, "app.import", Decision::Allow, trace_id)
            .await;
        Ok(entry)
    }

    pub(crate) async fn require_artifact_owner(
        &self,
        ctx: &AuthContext,
        app_id: Uuid,
    ) -> Result<(), AppPlatformError> {
        let obj = ctx.ns().artifact(&app_id.to_string());
        let ok = self
            .authz
            .check(
                &ctx.subject(),
                Relation::Owner,
                &obj,
                authz::Consistency::HigherConsistency,
            )
            .await
            .map_err(|e| AppPlatformError::Internal(format!("authz: {e}")))?;
        if !ok {
            return Err(AppPlatformError::Forbidden);
        }
        Ok(())
    }

    /// 補償: 作成済みテーブルの削除＋tuple 撤去（best-effort・欠落は tracing）。
    pub(crate) async fn compensate_tables(
        &self,
        ctx: &AuthContext,
        created: &[Uuid],
        trace_id: Option<&str>,
    ) {
        for id in created {
            if let Err(e) = self.data.delete_table(ctx, *id, trace_id).await {
                tracing::error!(error = %e, table_id = %id, "インストール補償のテーブル削除に失敗");
            }
            let obj = ctx.ns().data_table(&id.to_string());
            if let Err(e) = self.authz.delete_object_tuples(&obj).await {
                tracing::error!(error = %e, table_id = %id, "インストール補償の tuple 撤去に失敗");
            }
        }
    }

    pub(crate) async fn disable_clients_best_effort(&self, b1: Option<&str>, b2: Option<&str>) {
        let Some(oauth) = &self.oauth else { return };
        for id in [b1, b2].into_iter().flatten() {
            if let Err(e) = oauth.set_enabled(id, false).await {
                tracing::warn!(error = %e, client_id = id, "client 無効化に失敗");
            }
        }
    }

    pub(crate) async fn record_installed_event(
        &self,
        ctx: &AuthContext,
        app_id: Uuid,
        manifest: &MiniAppManifest,
        tables: &[Uuid],
        trace_id: Option<&str>,
    ) {
        self.record_audit(ctx, app_id, "app.install", Decision::Allow, trace_id)
            .await;
        self.emit_app_event(
            ctx,
            app_id,
            "app.installed",
            serde_json::json!({ "name": manifest.name, "version": manifest.version, "tables": tables }),
            trace_id,
        )
        .await;
    }

    pub(crate) async fn record_audit(
        &self,
        ctx: &AuthContext,
        app_id: Uuid,
        action: &'static str,
        decision: Decision,
        trace_id: Option<&str>,
    ) {
        if let Err(e) = self
            .audit
            .record(
                ctx,
                AuditEntry {
                    action,
                    object_type: "miniapp",
                    object_id: &app_id.to_string(),
                    decision,
                    trace_id,
                    metadata: serde_json::json!({ "security": decision == Decision::Deny }),
                },
            )
            .await
        {
            tracing::warn!(error = %e, "インストール監査の記録に失敗");
        }
    }

    pub(crate) async fn audit_deny(
        &self,
        ctx: &AuthContext,
        app_id: Uuid,
        action: &'static str,
        trace_id: Option<&str>,
    ) {
        self.record_audit(ctx, app_id, action, Decision::Deny, trace_id)
            .await;
    }

    /// outbox へアプリライフサイクルイベントを発行する（best-effort・単発 Tx）。
    pub(crate) async fn emit_app_event(
        &self,
        ctx: &AuthContext,
        app_id: Uuid,
        event_type: &str,
        mut payload: serde_json::Value,
        trace_id: Option<&str>,
    ) {
        if let Some(obj) = payload.as_object_mut() {
            obj.insert("event_type".into(), serde_json::json!(event_type));
            obj.insert("app_id".into(), serde_json::json!(app_id));
        }
        let result = async {
            let mut tx = self.db.begin().await?;
            emit_on(
                &mut tx,
                ctx,
                WriteEvent {
                    node_id: app_id,
                    version: 1,
                    op: WriteOp::Update,
                    payload,
                },
                trace_id,
            )
            .await
            .map_err(|e| sqlx::Error::Protocol(e.to_string()))?;
            tx.commit().await
        }
        .await;
        if let Err(e) = result {
            tracing::warn!(error = %e, event_type, "outbox 発行に失敗");
        }
    }
}
