//! 同意インストール（Task 9.13b）の [`app_platform::InstallService`] の組み立てと、起動時のクライアントの照合
//! （BUG-172）。
//!
//! main の配線と結合テストが同じ組み立てを使う（B1 のクライアントの戻り先を設定から求める経路をテストで通す）。

use std::sync::Arc;
use std::time::Duration;

use authz::AuthzClient;

use crate::config::AppConfig;

/// 同意インストールの InstallService を組む。
///
/// Keycloak admin（provisioner 資格情報＋admin base）が構成済みなら client 登録も配線する。
/// 未構成（dev/テスト）は登録スキップ（InstallService 側で warn）。B1 のクライアントの戻り先は web シェルの
/// ポップアップの callback（[`AppConfig::miniapp_b1_redirect_uris`]）。
pub fn build_install_service(
    config: &AppConfig,
    http: &reqwest::Client,
    db: &sqlx::PgPool,
    authz: &Arc<dyn AuthzClient>,
    mini_app_code: &Arc<app_platform::MiniAppCodeStore>,
    data_store: &Arc<data::DataStore>,
    secrets: Option<&Arc<secrets::SecretStore>>,
) -> app_platform::InstallService {
    let oauth = if let (Some(base), Some((id, secret))) = (
        config.auth.admin_base(),
        config.auth.provisioner_credentials(),
    ) {
        Some(app_gateway::OAuthClient::new(
            http.clone(),
            base,
            config.auth.token_endpoint(),
            id.to_string(),
            secret.to_string(),
        ))
    } else {
        tracing::info!("Keycloak admin 未構成: インストール時の client 登録は無効（dev）");
        None
    };
    let token_host = url::Url::parse(&config.auth.token_endpoint())
        .ok()
        .and_then(|u| u.host_str().map(str::to_string));
    app_platform::InstallService::new(
        db.clone(),
        app_platform::Registry::new(db.clone()),
        Arc::clone(mini_app_code),
        Arc::clone(data_store),
        authz.clone(),
        oauth,
        config.miniapp_b1_redirect_uris(),
    )
    .with_secrets(secrets.cloned(), token_host)
}

/// 照合を試し直すまでの間（Keycloak が起動の途中など）。
const REPAIR_RETRY: Duration = Duration::from_mins(5);
/// 短い間で試す回数（その後は [`REPAIR_RETRY_LATER`] おきに試し続ける）。
const REPAIR_ATTEMPTS: u32 = 12;
/// 短い間で揃えられなかったときに、試し続ける間（Keycloak が長く落ちていたとき・GAP-70）。
const REPAIR_RETRY_LATER: Duration = Duration::from_hours(1);

/// 前の版で登録したアプリのクライアントを、今の登録の形に揃える常駐の処理を起動する（BUG-172）。
///
/// 起動を待たせないように裏で動かす。揃えられないもの（Keycloak の失敗）が残ったら、[`REPAIR_RETRY`] おきに
/// [`REPAIR_ATTEMPTS`] 回試し、その後も揃うまで [`REPAIR_RETRY_LATER`] おきに試し続ける（これまでは次の起動まで
/// 止まった・GAP-70）。Keycloak の管理が未配線なら何もしない。
pub fn spawn_client_repair(installs: Arc<app_platform::InstallService>) {
    tokio::spawn(async move {
        for attempt in 1u32.. {
            match installs.repair_oauth_clients().await {
                Ok(p) if p.failed == 0 => {
                    if p.repaired + p.missing > 0 {
                        tracing::info!(
                            repaired = p.repaired,
                            recreated = p.recreated,
                            missing = p.missing,
                            "アプリのクライアントを今の登録の形に揃えました"
                        );
                    }
                    return;
                }
                Ok(p) => {
                    tracing::warn!(
                        repaired = p.repaired,
                        recreated = p.recreated,
                        missing = p.missing,
                        failed = p.failed,
                        attempt,
                        "アプリのクライアントの一部を揃えられません（試し直す）"
                    );
                }
                Err(e) => {
                    tracing::warn!(error = %e, attempt, "アプリのクライアントの照合に失敗（試し直す）");
                }
            }
            let wait = if attempt < REPAIR_ATTEMPTS {
                REPAIR_RETRY
            } else {
                REPAIR_RETRY_LATER
            };
            tokio::time::sleep(wait).await;
        }
    });
}
