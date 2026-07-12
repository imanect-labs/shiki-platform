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
use storage::audit::AuditRecorder;
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
    pub(crate) db: PgPool,
    pub(crate) registry: Registry,
    pub(crate) code: Arc<MiniAppCodeStore>,
    pub(crate) data: Arc<DataStore>,
    pub(crate) authz: Arc<dyn AuthzClient>,
    pub(crate) installations: AppInstallationStore,
    pub(crate) keys: TrustedKeyStore,
    pub(crate) oauth: Option<OAuthClient>,
    pub(crate) audit: AuditRecorder,
    /// B1 public client の redirect URI（ホスト支援 PKCE のシェル callback・PR10 が消費）。
    pub(crate) b1_redirect_uris: Vec<String>,
    /// B2 confidential secret の保管先（宛先束縛・未配線は保管スキップ＝トリガ無効）。
    pub(crate) secrets: Option<Arc<secrets::SecretStore>>,
    /// secret の宛先束縛に使う token endpoint ホスト。
    pub(crate) token_host: Option<String>,
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
            secrets: None,
            token_host: None,
        }
    }

    /// B2 confidential secret の保管先を配線する（Task 9.12・宛先束縛=token endpoint）。
    #[must_use]
    pub fn with_secrets(
        mut self,
        secrets: Option<Arc<secrets::SecretStore>>,
        token_host: Option<String>,
    ) -> Self {
        self.secrets = secrets;
        self.token_host = token_host;
        self
    }

    pub fn trusted_keys(&self) -> &TrustedKeyStore {
        &self.keys
    }

    pub fn installations(&self) -> &AppInstallationStore {
        &self.installations
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
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
                    frontend_bundle: manifest.frontend.as_ref().map(|f| f.sha256.as_str()),
                    server_bundle: manifest
                        .server
                        .as_ref()
                        .and_then(|s| s.code_sha256.as_deref()),
                    server_spec: manifest
                        .server
                        .as_ref()
                        .and_then(|s| serde_json::to_value(s).ok()),
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

        // B2: secret 保管（宛先束縛）＋ event/cron トリガの実体化（Task 9.12）。
        if let Some(secret) = secret_b2.as_deref() {
            self.store_b2_secret(ctx, app_id, secret).await;
        }
        if let Some(spec) = &manifest.server {
            if let Err(e) = self.provision_triggers(ctx, app_id, spec).await {
                self.compensate_tables(ctx, &created, trace_id).await;
                self.disable_clients_best_effort(client_b1.as_deref(), client_b2.as_deref())
                    .await;
                return Err(e);
            }
        }

        // ⑦ 監査＋outbox（app.installed・B2 トリガ/UI 更新の種）。
        self.record_installed_event(ctx, app_id, &manifest, &created, trace_id)
            .await;
        Ok(Installed {
            installation,
            table_ids: created,
            client_secret_b2: secret_b2,
        })
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
}

/// granted ⊆ requested（両方 CapabilityScope 閉集合でパース・未知は fail-closed）。
pub(crate) fn validate_granted(
    granted: &[String],
    requested: &[String],
) -> Result<(), AppPlatformError> {
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
pub(crate) fn map_gateway(e: app_gateway::GatewayError) -> AppPlatformError {
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
