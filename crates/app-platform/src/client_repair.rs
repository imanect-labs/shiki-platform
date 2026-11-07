//! 前の版で登録したアプリのクライアント（Keycloak）を、起動時に今の登録の形に揃える（BUG-172）。
//!
//! 前の版のインストールは、B1 のクライアントの戻り先に BFF のログインの戻り先を登録し、既定のスコープ（sub を載せる
//! `basic`）と tenant・groups のマッパーを付けていなかった。そのアプリはゲートウェイを呼べない（トークンが出ないか、
//! 出ても 401）。再インストールすれば直るが、利用者には分からない。そこでサーバーの起動時に、有効なインストールの
//! うち、今の登録の形の指紋（[`app_gateway::client_spec_fingerprint`]・B1 の戻り先を含む）で揃えていないものの
//! クライアントを [`app_gateway::OAuthClient::repair`] で揃え、指紋を書く（migration 0133）。指紋が同じなら
//! 何もしないので、2 回目からの起動では Keycloak を呼ばない。B2 のサービスアカウントの利用者（自動実行のトークンの
//! sub）も、このときインストールへ書く（BUG-183・migration 0135）。
//!
//! - 揃えるのは有効なインストールのクライアントだけ（アンインストールしたものは無効のまま残す）。
//! - Keycloak に無くなった B1（資格情報を持たない公開のクライアント）は作り直す（GAP-70）。作り直すのは、
//!   インストールが決めた形の id（`app-<app_id>-b1`）のクライアントだけ。
//! - 無くなった B2（secret を持つクライアント）は作り直さない。管理者が漏れた secret を無効にするために消したのなら、
//!   作り直すとアプリの主体（owner@miniapp）が同じ secret のまま戻る。新しい secret で作り直すには、インストールした
//!   人が持つ secret を書き換える権限が要る。入れ直せば新しい secret で作り、保管し直す。
//! - 作り直さない・作り直せない（B2・決めた形の id でない）ものは、`oauth_client_missing` の印を付けて warn に残す
//!   （インストールの画面が入れ直しが要ると知らせる・migration 0136）。印のある行は、起動のたびに試し直す（B1 だけが
//!   無かったなら、作り直して印を外す）。
//! - Keycloak で無効にされた B2 は、有効に戻さない（同じ secret のままアプリの主体が戻るのを避ける・BUG-201・D-137）。
//!   `oauth_client_disabled` の印を付け、自動実行の主体（`b2_service_sub`）を外して warn に残す（migration 0140）。
//!   印のある行は起動のたびに試し直し、管理者が Keycloak で有効に戻していたら揃えて印を外す。無効にされた B1 は、
//!   secret を持たないので、これまでどおり有効に戻す。入れ直し（利用者の明示の操作）は、無効の B2 の secret を作り直して
//!   から有効に戻す（[`app_gateway::OAuthClient::register`]・止めた secret では戻らない）。
//! - Keycloak の失敗で揃えられなかったものは印を付けず、この回の残りでは飛ばす（次の回でやり直す）。

use app_gateway::{ClientKind, ClientMarks, ClientsToRepair, OAuthClient};

use crate::install::map_gateway;
use crate::{AppPlatformError, InstallService};

/// 1 回に引くインストールの数。
const REPAIR_BATCH: i64 = 50;

/// 照合 1 回の結果。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClientRepairProgress {
    /// クライアントを今の形に揃えて印を付けたインストールの数（作り直したものを含む）。
    pub repaired: u64,
    /// Keycloak に無くなった B1 を作り直したインストールの数（GAP-70）。
    pub recreated: u64,
    /// Keycloak に無いクライアントを作り直さなかった・作り直せなかったインストールの数（B2 が無いものを含む・
    /// 入れ直しが要る印を付ける）。
    pub missing: u64,
    /// Keycloak で無効にされた B2 を、有効に戻さなかったインストールの数（BUG-201・印を付ける）。
    pub disabled: u64,
    /// 揃えられなかったインストールの数（印を付けず、次の回でやり直す）。
    pub failed: u64,
}

/// 1 件のインストールのクライアントを揃えた結果。
#[derive(Debug, Default)]
struct RowRepair {
    /// B2 のサービスアカウントの利用者（自動実行のトークンの sub）。
    b2_service_sub: Option<String>,
    /// 作り直したクライアントがある（B1 だけ）。
    recreated: bool,
    /// 作り直さない・作り直せないクライアントがある（入れ直しが要る）。
    missing: bool,
    /// Keycloak で無効にされた B2 を、有効に戻さなかった。
    disabled: bool,
}

impl InstallService {
    /// 今の登録の形の指紋（このサーバーの B1 の戻り先と、ゲートウェイの audience で求める）。
    pub(crate) fn client_spec(&self) -> String {
        let audience = self
            .oauth
            .as_ref()
            .map_or(app_gateway::DEFAULT_GATEWAY_AUDIENCE, OAuthClient::audience);
        app_gateway::client_spec_fingerprint(&self.b1_redirect_uris, audience)
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
                        ClientMarks {
                            missing: repaired.missing,
                            disabled: repaired.disabled,
                        },
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
                        "Keycloak にアプリのクライアントが無く、作り直しません（B2 は secret を持つので作り直さない・入れ直しが要る）");
                    progress.missing += 1;
                }
                if repaired.disabled {
                    tracing::warn!(app_id = %row.app_id, tenant_id = %row.tenant_id,
                        "Keycloak で無効にされたアプリの B2 を有効に戻しません（同じ secret のまま戻さない・BUG-201）");
                    progress.disabled += 1;
                }
                if repaired.missing || repaired.disabled {
                    skip.push(row.app_id);
                } else {
                    progress.repaired += 1;
                }
            }
        }
    }

    /// 1 件のインストールの B1・B2 のクライアントを揃える。無くなった B1 は作り直し、無くなった B2 は `missing`、
    /// 無効にされた B2 は `disabled` にする（どちらも有効にしない）。
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
                        .recreate_public(
                            b1,
                            &row.app_name,
                            &self.b1_redirect_uris,
                            row.client_id_b2.as_deref(),
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
            match found {
                // 無効にされた B2 は有効に戻さない（自動実行の主体も外す・モジュールの説明）。
                Some(repaired) if repaired.disabled => out.disabled = true,
                Some(repaired) => out.b2_service_sub = repaired.service_account_sub,
                // secret を持つクライアントは作り直さない（モジュールの説明）。
                None => out.missing = true,
            }
        }
        Ok(out)
    }
}
