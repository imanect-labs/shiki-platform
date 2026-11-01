//! ミニアプリのシェル（web）のオリジンと、B1 のクライアントの戻り先（BUG-172）。
//!
//! B1 のアプリのトークンは、web シェルがポップアップで取る（ホスト支援 PKCE・`web/src/lib/miniapp-oauth.ts`）。
//! ポップアップの戻り先は `{web のオリジン}/apps/oauth/callback` で、インストールはこれを B1 のクライアントの
//! 戻り先として Keycloak に登録する（前の版は BFF のログインの戻り先 `auth.redirect_uri` を登録していて、Keycloak が
//! シェルの要求を「無効なパラメーター: redirect_uri」で断っていた）。

use super::AppConfig;

/// web シェルのホスト支援 PKCE のポップアップの戻り先のパス（`web/src/lib/miniapp-oauth.ts` の `redirectUri` と同じ）。
pub const MINIAPP_OAUTH_CALLBACK_PATH: &str = "/apps/oauth/callback";

impl AppConfig {
    /// web シェルのオリジン（B1 の配信の frame-ancestors と、B1 のクライアントの戻り先の元）。
    ///
    /// `gateway.web_origin` があればそれ（末尾の `/` は外す）。無ければ `auth.redirect_uri`（BFF のログインの戻り先・
    /// web と同じオリジン）のオリジン。オリジンはブラウザーの `location.origin` と同じ形（既定のポートは書かない）。
    /// どちらも読めなければ dev の既定 `http://localhost:3000`。
    pub fn web_shell_origin(&self) -> String {
        if let Some(origin) = self.gateway.web_origin.as_deref() {
            return origin.trim_end_matches('/').to_string();
        }
        url::Url::parse(&self.auth.redirect_uri)
            .ok()
            .map(|u| u.origin())
            .filter(url::Origin::is_tuple)
            .map_or_else(
                || "http://localhost:3000".to_string(),
                |o| o.ascii_serialization(),
            )
    }

    /// B1 のクライアントの戻り先（web シェルのポップアップの callback）。
    pub fn miniapp_b1_redirect_uris(&self) -> Vec<String> {
        vec![format!(
            "{}{MINIAPP_OAUTH_CALLBACK_PATH}",
            self.web_shell_origin()
        )]
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::valid_config as sample_config;

    /// 戻り先は BFF のログインの戻り先ではなく、シェルのポップアップの callback（BUG-172）。
    #[test]
    fn b1_redirect_is_the_shell_popup_callback() {
        let mut cfg = sample_config();
        cfg.auth.redirect_uri = "http://localhost:3000/auth/callback".into();
        cfg.gateway.web_origin = None;
        assert_eq!(cfg.web_shell_origin(), "http://localhost:3000");
        assert_eq!(
            cfg.miniapp_b1_redirect_uris(),
            ["http://localhost:3000/apps/oauth/callback"]
        );
        assert!(!cfg
            .miniapp_b1_redirect_uris()
            .contains(&cfg.auth.redirect_uri));

        // 既定のポートは書かない（ブラウザーの location.origin と同じ形でないと Keycloak が照合しない）。
        cfg.auth.redirect_uri = "https://shiki.example:443/auth/callback".into();
        assert_eq!(
            cfg.miniapp_b1_redirect_uris(),
            ["https://shiki.example/apps/oauth/callback"]
        );

        // web のオリジンの設定が優先（末尾の / は外す）。
        cfg.gateway.web_origin = Some("https://apps.shiki.example/".into());
        assert_eq!(
            cfg.miniapp_b1_redirect_uris(),
            ["https://apps.shiki.example/apps/oauth/callback"]
        );

        // 読めない戻り先は dev の既定。
        cfg.gateway.web_origin = None;
        cfg.auth.redirect_uri = "not a url".into();
        assert_eq!(cfg.web_shell_origin(), "http://localhost:3000");
    }
}
