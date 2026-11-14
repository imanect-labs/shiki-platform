//! もう入れられないアプリのクライアントを Keycloak から消す（GAP-69a・[`super`] の子のモジュール）。
//!
//! アンインストールはクライアントを無効にするだけ（入れ直すと同じ client_id を有効に戻す）。アプリの実体
//! （mini_app_code のアーティファクト）を消すと入れ直せないので、無効のクライアントが Keycloak にたまり続ける。
//! そのクライアントを消す口。消してよいかの判断（アーティファクトが消され、有効なインストールも無い）は呼び出し側
//! （`app_platform::InstallService`）が行う。provisioner の `manage-clients` の権限で消せる（権限は足さない）。
//!
//! `oauth.rs` が 1000 行の上限に近いので、`OAuthClient` の private の欄と関数（`admin_token`・`find_internal_id`）
//! を使える子のモジュールとして分けている。

use super::OAuthClient;
use crate::GatewayError;

impl OAuthClient {
    /// クライアントを消す（B1・B2 のどちらにも使う）。消したら `true`、もう無ければ `false`（どちらも成功）。
    ///
    /// 探してから消すまでの間にほかの経路が消していた（404）ときも、もう無いものとして `false` を返す。
    pub async fn delete(&self, client_id: &str) -> Result<bool, GatewayError> {
        let token = self.admin_token().await?;
        let internal_id = match self.find_internal_id(&token, client_id).await {
            Ok(id) => id,
            Err(GatewayError::NotFound) => return Ok(false),
            Err(e) => return Err(e),
        };
        let resp = self
            .http
            .delete(format!("{}/clients/{internal_id}", self.admin_base))
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|e| GatewayError::Upstream(format!("client 削除: {e}")))?;
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(false);
        }
        if !status.is_success() {
            return Err(GatewayError::Upstream(format!("client 削除応答: {status}")));
        }
        Ok(true)
    }
}
