//! 前の版で登録したアプリのクライアント（Keycloak）を、起動時に今の登録の形に揃える（BUG-172）。
//!
//! 前の版のインストールは、B1 のクライアントの戻り先に BFF のログインの戻り先を登録し、既定のスコープ（sub を載せる
//! `basic`）と tenant・groups のマッパーを付けていなかった。そのアプリはゲートウェイを呼べない（トークンが出ないか、
//! 出ても 401）。再インストールすれば直るが、利用者には分からない。そこでサーバーの起動時に、有効なインストールの
//! うち、今の登録の形の指紋（[`app_gateway::client_spec_fingerprint`]・B1 の戻り先を含む）で揃えていないものの
//! クライアントを [`app_gateway::OAuthClient::repair`] で揃え、指紋を書く（migration 0124）。指紋が同じなら
//! 何もしないので、2 回目からの起動では Keycloak を呼ばない。B2 のサービスアカウントの利用者（自動実行のトークンの
//! sub）も、このときインストールへ書く（BUG-183・migration 0126）。
//!
//! - 揃えるのは有効なインストールのクライアントだけ（アンインストールしたものは無効のまま残す）。
//! - Keycloak に無くなったクライアントは作り直す（GAP-70）。B1 はそのまま作り、B2 は保管してある secret（アプリの
//!   主体の can_use で読む）で作る（新しい secret を作らないので、保管し直さずに済む）。作り直すのは、インストールが
//!   決めた形の id（`app-<app_id>-b1` / `-b2`）のクライアントだけ。
//! - 作り直せない（secrets が未構成・secret が無い・決めた形の id でない）ものは、`oauth_client_missing` の印を
//!   付けて warn に残す（インストールの画面が入れ直しが要ると知らせる・migration 0127）。印のある行は、起動のたびに
//!   試し直す。
//! - Keycloak の失敗で揃えられなかったものは印を付けず、この回の残りでは飛ばす（次の回でやり直す）。

use app_gateway::{ClientKind, ClientsToRepair, OAuthClient};
use authz::AuthContext;

use crate::install::map_gateway;
use crate::install_ops::b2_secret_name;
use crate::{AppPlatformError, InstallService};

/// 1 回に引くインストールの数。
const REPAIR_BATCH: i64 = 50;

/// 照合 1 回の結果。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClientRepairProgress {
    /// クライアントを今の形に揃えて印を付けたインストールの数（作り直したものを含む）。
    pub repaired: u64,
    /// Keycloak に無くなったクライアントを作り直したインストールの数（GAP-70）。
    pub recreated: u64,
    /// Keycloak に無いクライアントを作り直せなかったインストールの数（入れ直しが要る印を付ける）。
    pub missing: u64,
    /// 揃えられなかったインストールの数（印を付けず、次の回でやり直す）。
    pub failed: u64,
}

/// 1 件のインストールのクライアントを揃えた結果。
#[derive(Debug, Default)]
struct RowRepair {
    /// B2 のサービスアカウントの利用者（自動実行のトークンの sub）。
    b2_service_sub: Option<String>,
    /// 作り直したクライアントがある。
    recreated: bool,
    /// 作り直せないクライアントがある（入れ直しが要る）。
    missing: bool,
}

impl InstallService {
    /// 今の登録の形の指紋（このサーバーの B1 の戻り先で求める）。
    pub(crate) fn client_spec(&self) -> String {
        app_gateway::client_spec_fingerprint(&self.b1_redirect_uris)
    }

    /// 有効なインストールのうち、クライアントを今の形で揃えていないものと、前の照合で作り直せなかったものを揃える
    /// （全テナント・冪等）。
    ///
    /// Keycloak の管理（provisioner）が未配線なら何もしない。
    pub async fn repair_oauth_clients(&self) -> Result<ClientRepairProgress, AppPlatformError> {
        let mut progress = ClientRepairProgress::default();
        let Some(oauth) = &self.oauth else {
            return Ok(progress);
        };
        let spec = self.client_spec();
        // この回で揃えられなかったもの・作り直せなかったもの（印があっても引き直さない）。
        let mut skip = Vec::new();
        loop {
            let batch = self
                .installations
                .clients_to_repair(&spec, &skip, REPAIR_BATCH)
                .await
                .map_err(map_gateway)?;
            if batch.is_empty() {
                return Ok(progress);
            }
            for row in batch {
                let repaired = match self.repair_clients_of(oauth, &row).await {
                    Ok(repaired) => repaired,
                    Err(e) => {
                        tracing::warn!(error = %e, app_id = %row.app_id, tenant_id = %row.tenant_id,
                            "アプリのクライアントを今の形に揃えられません（次の回でやり直す）");
                        progress.failed += 1;
                        skip.push(row.app_id);
                        continue;
                    }
                };
                if let Err(e) = self
                    .installations
                    .mark_clients_repaired(
                        &row.tenant_id,
                        row.app_id,
                        &spec,
                        repaired.b2_service_sub.as_deref(),
                        repaired.missing,
                    )
                    .await
                {
                    tracing::warn!(error = %e, app_id = %row.app_id, "クライアントを揃えた印を付けられません");
                    progress.failed += 1;
                    skip.push(row.app_id);
                    continue;
                }
                if repaired.recreated {
                    progress.recreated += 1;
                }
                if repaired.missing {
                    tracing::warn!(app_id = %row.app_id, tenant_id = %row.tenant_id,
                        "Keycloak にアプリのクライアントが無く、作り直せません（入れ直しが要る）");
                    progress.missing += 1;
                    skip.push(row.app_id);
                } else {
                    progress.repaired += 1;
                }
            }
        }
    }

    /// 1 件のインストールの B1・B2 のクライアントを揃える。無くなったものは作り直す（作れなければ `missing`）。
    async fn repair_clients_of(
        &self,
        oauth: &OAuthClient,
        row: &ClientsToRepair,
    ) -> Result<RowRepair, AppPlatformError> {
        let mut out = RowRepair::default();
        if let Some(b1) = &row.client_id_b1 {
            let found = oauth
                .repair(
                    ClientKind::PublicPkce,
                    b1,
                    &row.app_name,
                    &self.b1_redirect_uris,
                    row.client_id_b2.as_deref(),
                )
                .await
                .map_err(map_gateway)?;
            if found.is_none() {
                if *b1 == format!("app-{}-b1", row.app_id) {
                    oauth
                        .recreate(
                            ClientKind::PublicPkce,
                            b1,
                            &row.app_name,
                            &self.b1_redirect_uris,
                            row.client_id_b2.as_deref(),
                            None,
                        )
                        .await
                        .map_err(map_gateway)?;
                    out.recreated = true;
                } else {
                    out.missing = true;
                }
            }
        }
        if let Some(b2) = &row.client_id_b2 {
            let found = oauth
                .repair(ClientKind::Confidential, b2, &row.app_name, &[], None)
                .await
                .map_err(map_gateway)?;
            if let Some(repaired) = found {
                out.b2_service_sub = repaired.service_account_sub;
                return Ok(out);
            }
            let secret = if *b2 == format!("app-{}-b2", row.app_id) {
                self.stored_b2_secret(row).await
            } else {
                None
            };
            if let Some(secret) = secret {
                let recreated = oauth
                    .recreate(
                        ClientKind::Confidential,
                        b2,
                        &row.app_name,
                        &[],
                        None,
                        Some(&secret),
                    )
                    .await
                    .map_err(map_gateway)?;
                out.b2_service_sub = recreated.service_account_sub;
                out.recreated = true;
            } else {
                out.missing = true;
            }
        }
        Ok(out)
    }

    /// 保管してある B2 の secret（アプリの主体の can_use で読む・宛先は Keycloak のトークンのエンドポイント）。
    ///
    /// secrets が未構成・secret が無い・読めないときは `None`（作り直せない）。
    async fn stored_b2_secret(&self, row: &ClientsToRepair) -> Option<String> {
        let store = self.secrets.as_ref()?;
        let app_ctx = AuthContext::for_miniapp(
            row.tenant_id.clone(),
            row.org.clone(),
            &row.app_id.to_string(),
        );
        match store
            .resolve(
                &app_ctx,
                &b2_secret_name(row.app_id),
                self.token_host.as_deref(),
                None,
            )
            .await
        {
            Ok(resolved) => String::from_utf8(resolved.plaintext).ok(),
            Err(e) => {
                tracing::warn!(error = %e, app_id = %row.app_id, "B2 の secret を読めないので作り直せません");
                None
            }
        }
    }
}
