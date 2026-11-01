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
//! - Keycloak に無いクライアントは作らない（confidential の secret を保管し直す経路がここに無い）。印を付けて
//!   warn に残す（再インストールで作り直す）。
//! - Keycloak の失敗で揃えられなかったものは印を付けず、この回の残りでは飛ばす（次の回でやり直す）。

use app_gateway::{ClientKind, ClientsToRepair, OAuthClient};

use crate::install::map_gateway;
use crate::{AppPlatformError, InstallService};

/// 1 回に引くインストールの数。
const REPAIR_BATCH: i64 = 50;

/// 照合 1 回の結果。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClientRepairProgress {
    /// クライアントを今の形に揃えて印を付けたインストールの数。
    pub repaired: u64,
    /// Keycloak にクライアントが無かったインストールの数（印は付ける・再インストールで作り直す）。
    pub missing: u64,
    /// 揃えられなかったインストールの数（印を付けず、次の回でやり直す）。
    pub failed: u64,
}

impl InstallService {
    /// 今の登録の形の指紋（このサーバーの B1 の戻り先で求める）。
    pub(crate) fn client_spec(&self) -> String {
        app_gateway::client_spec_fingerprint(&self.b1_redirect_uris)
    }

    /// 有効なインストールのうち、クライアントを今の形で揃えていないものを揃える（全テナント・冪等）。
    ///
    /// Keycloak の管理（provisioner）が未配線なら何もしない。
    pub async fn repair_oauth_clients(&self) -> Result<ClientRepairProgress, AppPlatformError> {
        let mut progress = ClientRepairProgress::default();
        let Some(oauth) = &self.oauth else {
            return Ok(progress);
        };
        let spec = self.client_spec();
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
                let (found, b2_service_sub) = match self.repair_clients_of(oauth, &row).await {
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
                        b2_service_sub.as_deref(),
                    )
                    .await
                {
                    tracing::warn!(error = %e, app_id = %row.app_id, "クライアントを揃えた印を付けられません");
                    progress.failed += 1;
                    skip.push(row.app_id);
                    continue;
                }
                if found {
                    progress.repaired += 1;
                } else {
                    tracing::warn!(app_id = %row.app_id, tenant_id = %row.tenant_id,
                        "Keycloak にアプリのクライアントがありません（再インストールで作り直す）");
                    progress.missing += 1;
                }
            }
        }
    }

    /// 1 件のインストールの B1・B2 のクライアントを揃える（どちらも Keycloak にあれば `true`）。B2 があれば、その
    /// サービスアカウントの利用者（自動実行のトークンの sub・BUG-183）も返す。
    async fn repair_clients_of(
        &self,
        oauth: &OAuthClient,
        row: &ClientsToRepair,
    ) -> Result<(bool, Option<String>), AppPlatformError> {
        let mut found = true;
        if let Some(b1) = &row.client_id_b1 {
            found &= oauth
                .repair(
                    ClientKind::PublicPkce,
                    b1,
                    &row.app_name,
                    &self.b1_redirect_uris,
                    row.client_id_b2.as_deref(),
                )
                .await
                .map_err(map_gateway)?
                .is_some();
        }
        let mut b2_service_sub = None;
        if let Some(b2) = &row.client_id_b2 {
            match oauth
                .repair(ClientKind::Confidential, b2, &row.app_name, &[], None)
                .await
                .map_err(map_gateway)?
            {
                Some(repaired) => b2_service_sub = repaired.service_account_sub,
                None => found = false,
            }
        }
        Ok((found, b2_service_sub))
    }
}
