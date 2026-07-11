//! 同意インストール／プロビジョン／アンインストール（Task 9.13b）。
//!
//! **インストール認可 = mini_app_code アーティファクトの owner ReBAC**（human 確定判断・
//! miniapp 型に installer relation は作らない）。フローは:
//! ①レジストリ解決 ②信頼ティア検証（first-party は署名必須・marketplace は拒否）
//! ③granted ⊆ requested 検証 ④所有テーブル・プロビジョン（app_id 束縛＋FGA tuple:
//! owner@miniapp・管理者指定ロール viewer/editor）⑤Keycloak client 登録（B1/B2・9.7 再利用）
//! ⑥installation 行（AiPin をマニフェスト Budget/tools から焼き込み）⑦監査＋outbox。
//!
//! **補償**: 部分失敗時は作成済みテーブルを削除し FGA tuple を撤去する（installation 行は
//! 最後に書く＝失敗時は存在しない）。Keycloak client は enabled=false へ倒す（best-effort・
//! client_id は決定的なので再インストールで再利用される）。

use std::sync::Arc;

use app_gateway::{AiPin, AppInstallation, AppInstallationStore, NewAppInstallation, OAuthClient};
use authz::{AuthContext, AuthzClient, CapabilityScope, Relation};
use data::{DataStore, NewDataTable};
use sqlx::PgPool;
use storage::audit::{AuditEntry, AuditRecorder, Decision};
use storage::event::{emit_on, WriteEvent, WriteOp};
use uuid::Uuid;

use crate::manifest::TrustTier;
use crate::sign::verify_manifest_signature;
use crate::store::MiniAppCodeStore;
use crate::trusted_key::TrustedKeyStore;
use crate::{AppPlatformError, MiniAppManifest, Registry};

/// インストール要求（同意内容）。
#[derive(Debug, Clone)]
pub struct InstallRequest {
    pub name: String,
    pub version: String,
    /// 同意して付与するスコープ（requested の部分集合であること・fail-closed 検証）。
    pub granted_scopes: Vec<String>,
    /// プロビジョンしたテーブルへ viewer を付与するロール。
    pub viewer_roles: Vec<String>,
    /// プロビジョンしたテーブルへ editor を付与するロール。
    pub editor_roles: Vec<String>,
}

/// インストール結果。
#[derive(Debug, Clone, serde::Serialize)]
pub struct Installed {
    pub installation: AppInstallation,
    /// プロビジョンされたテーブル ID（manifest.tables 順）。
    pub table_ids: Vec<Uuid>,
    /// B2 confidential client の secret（生成時のみ・呼び出し側が secrets 保管へ回す）。
    pub client_secret_b2: Option<String>,
}

/// 同意インストールのオーケストレータ（単一チョークポイント）。
pub struct InstallService {
    db: PgPool,
    registry: Registry,
    code: Arc<MiniAppCodeStore>,
    data: Arc<DataStore>,
    authz: Arc<dyn AuthzClient>,
    installations: AppInstallationStore,
    keys: TrustedKeyStore,
    oauth: Option<OAuthClient>,
    audit: AuditRecorder,
    /// B1 public client の redirect URI（ホスト支援 PKCE のシェル callback・PR10 が消費）。
    b1_redirect_uris: Vec<String>,
}

impl InstallService {
    #[allow(clippy::too_many_arguments)] // 依存束の注入点（配線からの一回きり）。
    pub fn new(
        db: PgPool,
        registry: Registry,
        code: Arc<MiniAppCodeStore>,
        data: Arc<DataStore>,
        authz: Arc<dyn AuthzClient>,
        oauth: Option<OAuthClient>,
        b1_redirect_uris: Vec<String>,
    ) -> Self {
        let audit = AuditRecorder::new(db.clone());
        InstallService {
            installations: AppInstallationStore::new(db.clone()),
            keys: TrustedKeyStore::new(db.clone()),
            db,
            registry,
            code,
            data,
            authz,
            oauth,
            audit,
            b1_redirect_uris,
        }
    }

    pub fn trusted_keys(&self) -> &TrustedKeyStore {
        &self.keys
    }

    pub fn installations(&self) -> &AppInstallationStore {
        &self.installations
    }

    /// 同意インストール（本文はモジュール rustdoc の①〜⑦）。
    pub async fn install(
        &self,
        ctx: &AuthContext,
        req: InstallRequest,
        trace_id: Option<&str>,
    ) -> Result<Installed, AppPlatformError> {
        // ① レジストリ解決（yank 済みは新規インストール不可）。
        let entry = self
            .registry
            .get(ctx, "mini_app_code", &req.name, &req.version)
            .await?
            .ok_or(AppPlatformError::NotFound)?;
        if entry.yanked {
            return Err(AppPlatformError::Conflict(
                "このバージョンは yank されています".into(),
            ));
        }
        let app_id = entry.artifact_id;

        // インストール認可: mini_app_code アーティファクトの owner（human 確定判断）。
        self.require_artifact_owner(ctx, app_id).await?;

        // マニフェスト取得（owner なので viewer も満たす）。
        let (_, manifest) = self
            .code
            .get(ctx, app_id, Some(entry.artifact_version), trace_id)
            .await?;

        // ② 信頼ティア検証。
        self.verify_trust_tier(ctx, &manifest, &entry.trust_tier, app_id)
            .await?;

        // ③ granted ⊆ requested（語彙は CapabilityScope 閉集合・未知は fail-closed）。
        validate_granted(&req.granted_scopes, &manifest.requested_scopes)?;

        // ④ 所有テーブル・プロビジョン（＋FGA tuple）。失敗時は補償。
        let mut created: Vec<Uuid> = Vec::new();
        let result = self
            .provision(ctx, &manifest, app_id, &req, &mut created, trace_id)
            .await;
        let (client_b1, client_b2, secret_b2) = match result {
            Ok(clients) => clients,
            Err(e) => {
                self.compensate_tables(ctx, &created, trace_id).await;
                self.audit_deny(ctx, app_id, "app.install", trace_id).await;
                return Err(e);
            }
        };

        // ⑥ installation 行（AiPin = マニフェスト Budget/tools の焼き込み・同意時点でピン）。
        let installation = self
            .installations
            .upsert(
                ctx,
                NewAppInstallation {
                    app_id,
                    app_name: &manifest.name,
                    installed_version: &manifest.version,
                    granted_scopes: &req.granted_scopes,
                    client_id_b1: client_b1.as_deref(),
                    client_id_b2: client_b2.as_deref(),
                    ai: AiPin {
                        budget_models: manifest.budget.models.clone(),
                        budget_daily_usd_micros: manifest.budget.daily_usd_micros,
                        budget_max_tokens: manifest.budget.max_tokens,
                        agent_tools: manifest.tools.clone(),
                    },
                },
            )
            .await
            .map_err(map_gateway);
        let installation = match installation {
            Ok(i) => i,
            Err(e) => {
                self.compensate_tables(ctx, &created, trace_id).await;
                self.disable_clients_best_effort(client_b1.as_deref(), client_b2.as_deref())
                    .await;
                return Err(e);
            }
        };

        // ⑦ 監査＋outbox（app.installed・B2 トリガ/UI 更新の種）。
        self.record_installed_event(ctx, app_id, &manifest, &created, trace_id)
            .await;
        Ok(Installed {
            installation,
            table_ids: created,
            client_secret_b2: secret_b2,
        })
    }

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

    async fn require_artifact_owner(
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

    /// 信頼ティア: first-party=有効な信頼鍵の署名必須／in-house=owner 同意のみ／marketplace=拒否。
    async fn verify_trust_tier(
        &self,
        ctx: &AuthContext,
        manifest: &MiniAppManifest,
        tier: &str,
        app_id: Uuid,
    ) -> Result<(), AppPlatformError> {
        match tier {
            t if t == TrustTier::FirstParty.as_str() => {
                let sig = self
                    .registry
                    .signature_of(ctx, "mini_app_code", &manifest.name, &manifest.version)
                    .await?
                    .ok_or_else(|| {
                        AppPlatformError::Invalid(
                            "first-party アプリは署名付き publish が必要です".into(),
                        )
                    })?;
                let keys = self.keys.active_key_bytes(ctx).await?;
                let ok = keys
                    .iter()
                    .any(|k| verify_manifest_signature(manifest, &sig, k).is_ok());
                if !ok {
                    self.audit_deny(ctx, app_id, "app.install.signature", None)
                        .await;
                    return Err(AppPlatformError::Forbidden);
                }
                Ok(())
            }
            t if t == TrustTier::InHouse.as_str() => Ok(()),
            // marketplace は予約（審査トラック未実装）・未知ティアも fail-closed。
            _ => Err(AppPlatformError::Invalid(format!(
                "信頼ティア '{tier}' はインストールできません"
            ))),
        }
    }

    /// ④＋⑤: テーブル作成＋FGA tuple ＋ Keycloak client 登録。
    async fn provision(
        &self,
        ctx: &AuthContext,
        manifest: &MiniAppManifest,
        app_id: Uuid,
        req: &InstallRequest,
        created: &mut Vec<Uuid>,
        trace_id: Option<&str>,
    ) -> Result<(Option<String>, Option<String>, Option<String>), AppPlatformError> {
        let ns = ctx.ns();
        let miniapp_subject = ns.miniapp_principal(&app_id.to_string());
        for t in &manifest.tables {
            let table = self
                .data
                .create_table_for_app(
                    ctx,
                    NewDataTable {
                        name: t.name.clone(),
                        schema: t.schema.clone(),
                    },
                    app_id,
                    trace_id,
                )
                .await?;
            created.push(table.id);
            let obj = ns.data_table(&table.id.to_string());
            // owner@miniapp（アプリ自身の第一層 ReBAC・B2 サービス実行が使う）。
            self.authz
                .write_tuple(&miniapp_subject, Relation::Owner, &obj)
                .await
                .map_err(|e| AppPlatformError::Internal(format!("authz: {e}")))?;
            // 管理者指定ロールへ viewer/editor を付与（role#member userset）。
            for (roles, rel) in [
                (&req.viewer_roles, Relation::Viewer),
                (&req.editor_roles, Relation::Editor),
            ] {
                for role in roles {
                    let subject = authz::Subject::userset(&ns.role(role), Relation::Member);
                    self.authz
                        .write_tuple(&subject, rel, &obj)
                        .await
                        .map_err(|e| AppPlatformError::Internal(format!("authz: {e}")))?;
                }
            }
        }

        // ⑤ Keycloak client 登録（B1 は常時・B2 は server 宣言時のみ）。決定的 client_id
        // （app-<uuid>-b1/b2）＝再インストールは冪等（409 許容）。oauth 未配線（dev）は None。
        let Some(oauth) = &self.oauth else {
            tracing::warn!("Keycloak admin 未配線: client 登録をスキップします（dev モード）");
            return Ok((None, None, None));
        };
        let b1_id = format!("app-{app_id}-b1");
        oauth
            .register(
                app_gateway::ClientKind::PublicPkce,
                &b1_id,
                &manifest.name,
                &self.b1_redirect_uris,
            )
            .await
            .map_err(map_gateway)?;
        let (b2_id, b2_secret) = if manifest.server.is_some() {
            let id = format!("app-{app_id}-b2");
            let registered = oauth
                .register(
                    app_gateway::ClientKind::Confidential,
                    &id,
                    &manifest.name,
                    &[],
                )
                .await
                .map_err(map_gateway)?;
            (Some(id), registered.client_secret)
        } else {
            (None, None)
        };
        Ok((Some(b1_id), b2_id, b2_secret))
    }

    /// 補償: 作成済みテーブルの削除＋tuple 撤去（best-effort・欠落は tracing）。
    async fn compensate_tables(&self, ctx: &AuthContext, created: &[Uuid], trace_id: Option<&str>) {
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

    async fn disable_clients_best_effort(&self, b1: Option<&str>, b2: Option<&str>) {
        let Some(oauth) = &self.oauth else { return };
        for id in [b1, b2].into_iter().flatten() {
            if let Err(e) = oauth.set_enabled(id, false).await {
                tracing::warn!(error = %e, client_id = id, "client 無効化に失敗");
            }
        }
    }

    async fn record_installed_event(
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

    async fn record_audit(
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

    async fn audit_deny(
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
    async fn emit_app_event(
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

/// granted ⊆ requested（両方 CapabilityScope 閉集合でパース・未知は fail-closed）。
fn validate_granted(granted: &[String], requested: &[String]) -> Result<(), AppPlatformError> {
    let requested: Vec<CapabilityScope> = requested
        .iter()
        .map(|s| {
            CapabilityScope::parse_scope_string(s)
                .map_err(AppPlatformError::Invalid)
                .and_then(|v| {
                    v.into_iter().next().ok_or_else(|| {
                        AppPlatformError::Invalid("requested_scopes が空要素を含みます".into())
                    })
                })
        })
        .collect::<Result<_, _>>()?;
    for g in granted {
        let scope = CapabilityScope::parse_scope_string(g)
            .map_err(AppPlatformError::Invalid)?
            .into_iter()
            .next()
            .ok_or_else(|| AppPlatformError::Invalid("granted_scopes が空要素を含みます".into()))?;
        if !requested.contains(&scope) {
            return Err(AppPlatformError::Invalid(format!(
                "スコープ {} はマニフェストの requested_scopes に含まれていません",
                scope.as_str()
            )));
        }
    }
    Ok(())
}

#[allow(clippy::needless_pass_by_value)]
fn map_gateway(e: app_gateway::GatewayError) -> AppPlatformError {
    use app_gateway::GatewayError as G;
    match e {
        G::NotFound => AppPlatformError::NotFound,
        G::Forbidden(_) | G::Unauthenticated(_) => AppPlatformError::Forbidden,
        G::Invalid(m) => AppPlatformError::Invalid(m),
        G::Conflict(m) => AppPlatformError::Conflict(m),
        other => AppPlatformError::Internal(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::validate_granted;

    #[test]
    fn granted_must_be_subset_of_requested() {
        let requested = vec!["data.read".to_string(), "data.write".to_string()];
        assert!(validate_granted(&["data.read".into()], &requested).is_ok());
        assert!(validate_granted(&[], &requested).is_ok());
        // requested 外は拒否。
        assert!(validate_granted(&["rag.query".into()], &requested).is_err());
        // 未知スコープは granted/requested どちら側でも fail-closed。
        assert!(validate_granted(&["bogus.scope".into()], &requested).is_err());
        assert!(validate_granted(&["data.read".into()], &["bogus".into()]).is_err());
    }
}
