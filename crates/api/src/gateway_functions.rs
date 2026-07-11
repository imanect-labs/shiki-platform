//! ゲートウェイ B2 関数起動の port 実装（Task 9.12）。
//!
//! ユーザー起点: 呼出ユーザーの gateway トークンを **RFC 8693 token-exchange
//! （sub=ユーザー維持・confused-deputy 防御）** で B2 confidential client のトークンへ
//! 交換し、[`app_platform::FunctionRunner`]（wasm サンドボックス）のホスト委譲 Bearer に
//! 使う。B2 client secret は crates/secrets（宛先束縛=token endpoint）から解決し、
//! **ゲスト/レスポンスへは一切出さない**。

use std::sync::Arc;

use app_gateway::{FunctionInvokeSpec, FunctionPort, GatewayError};
use app_platform::{FunctionActor, FunctionInvocation, FunctionRunner};
use authz::AuthContext;

pub(crate) struct GatewayFunctionPort {
    pub runner: Arc<FunctionRunner>,
    pub http: reqwest::Client,
    pub token_endpoint: String,
    pub secrets: Option<Arc<secrets::SecretStore>>,
    pub gateway_audience: String,
    pub installations: app_gateway::AppInstallationStore,
}

impl GatewayFunctionPort {
    /// アクティブなインストール（トリガ用のピン解決）。
    pub(crate) async fn runner_installation(
        &self,
        tenant_id: &str,
        app_id: uuid::Uuid,
    ) -> anyhow::Result<Option<app_gateway::AppInstallation>> {
        self.installations
            .resolve_active_by_app(tenant_id, app_id)
            .await
            .map_err(|e| anyhow::anyhow!("installation: {e}"))
    }

    /// service identity のトークン（B2 client_credentials・event/cron 起動用）。
    ///
    /// 能力スコープは client の optional client scope なので、`scopes`（＝インストールの
    /// granted_scopes）を `scope` パラメタで明示要求する。これが無いと service token に
    /// data.* / notify.send 等が乗らず、関数内の能力呼び出しが二重ゲートで 403 になる。
    pub(crate) async fn client_credentials_token(
        &self,
        client_id: &str,
        client_secret: &str,
        scopes: &[String],
    ) -> Result<String, GatewayError> {
        #[derive(serde::Deserialize)]
        struct Tok {
            access_token: String,
        }
        let scope = scopes.join(" ");
        let mut form = vec![
            ("grant_type", "client_credentials"),
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("audience", self.gateway_audience.as_str()),
        ];
        if !scope.is_empty() {
            form.push(("scope", scope.as_str()));
        }
        let resp = self
            .http
            .post(&self.token_endpoint)
            .form(&form)
            .send()
            .await
            .map_err(|e| GatewayError::Upstream(format!("service token 取得: {e}")))?;
        if !resp.status().is_success() {
            return Err(GatewayError::Upstream(format!(
                "service token 応答: {}",
                resp.status()
            )));
        }
        let tok: Tok = resp
            .json()
            .await
            .map_err(|e| GatewayError::Upstream(format!("service token parse: {e}")))?;
        Ok(tok.access_token)
    }

    /// B2 secret を解決する（宛先束縛=token endpoint ホスト・fail-closed）。
    pub(crate) async fn resolve_b2_secret(
        &self,
        ctx: &AuthContext,
        app_id: uuid::Uuid,
    ) -> Result<String, GatewayError> {
        let Some(store) = &self.secrets else {
            return Err(GatewayError::Upstream(
                "secrets 未構成のため B2 関数を実行できません".into(),
            ));
        };
        let host = url::Url::parse(&self.token_endpoint)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string));
        let resolved = store
            .resolve(ctx, &format!("miniapp-b2-{app_id}"), host.as_deref(), None)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, %app_id, "B2 secret の解決に失敗");
                GatewayError::Upstream("B2 client secret を解決できません".into())
            })?;
        String::from_utf8(resolved.plaintext)
            .map_err(|_| GatewayError::Internal("B2 secret が UTF-8 ではありません".into()))
    }
}

#[async_trait::async_trait]
impl FunctionPort for GatewayFunctionPort {
    async fn invoke(
        &self,
        ctx: &AuthContext,
        spec: FunctionInvokeSpec,
    ) -> Result<serde_json::Value, GatewayError> {
        let Some(server_bundle) = spec.server_bundle.as_deref() else {
            return Err(GatewayError::Invalid(
                "このアプリにはサーバコードがありません".into(),
            ));
        };
        let Some(client_id_b2) = spec.client_id_b2.as_deref() else {
            return Err(GatewayError::Invalid("B2 client が未登録です".into()));
        };
        // B2 client secret はアプリ所有の資格情報なので、呼出ユーザーではなくアプリの
        // service identity（miniapp）で解決する（installer 以外のユーザーでも起動できる）。
        let app_ctx = AuthContext::for_miniapp(
            ctx.tenant_id.clone(),
            ctx.org.clone(),
            &spec.app_id.to_string(),
        );
        let secret = self.resolve_b2_secret(&app_ctx, spec.app_id).await?;
        // sub=ユーザーを維持したまま B2 クライアントのトークンへ交換（RFC 8693）。
        let exchanged = app_gateway::exchange_for_user(
            &self.http,
            &self.token_endpoint,
            client_id_b2,
            &secret,
            &spec.subject_token,
            &self.gateway_audience,
        )
        .await?;

        let egress = spec
            .server_spec
            .as_ref()
            .and_then(|s| s.get("egress_allowlist"))
            .and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok())
            .unwrap_or_default();
        let outcome = self
            .runner
            .run(
                server_bundle,
                FunctionInvocation {
                    tenant_id: ctx.tenant_id.clone(),
                    app_id: spec.app_id,
                    function: spec.function,
                    payload: spec.payload,
                    bearer: exchanged.access_token,
                    actor: FunctionActor::User,
                    egress_allowlist: egress,
                },
            )
            .await
            .map_err(map_platform)?;
        Ok(serde_json::json!({
            "ok": outcome.ok,
            "value": outcome.value,
            "logs": outcome.logs,
        }))
    }
}

#[allow(clippy::needless_pass_by_value)]
fn map_platform(e: app_platform::AppPlatformError) -> GatewayError {
    use app_platform::AppPlatformError as E;
    match e {
        E::NotFound => GatewayError::NotFound,
        E::Forbidden => GatewayError::Forbidden("この操作は許可されていません".into()),
        E::Invalid(m) => GatewayError::Invalid(m),
        E::Conflict(m) => GatewayError::Conflict(m),
        E::Internal(m) => {
            tracing::error!(error = %m, "B2 関数実行の内部エラー");
            GatewayError::Internal("内部エラーが発生しました".into())
        }
    }
}
