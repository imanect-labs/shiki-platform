//! 外への送信の中継の設定（`SHIKI__EGRESS__*`・GAP-72）。
//!
//! 外へ出るのに中継（フォワードプロキシ）が要る環境で、ワークフローの外部への送信（「外部 API 呼び出し」・
//! `Shiki.http.request`）・web.fetch・ミニアプリの関数の外部 HTTP を、ここに書いた中継に通す。宛先の確かめは弱めない
//! （名前は Shiki が 1 回だけ引いて確かめ、中継には確かめた IP アドレスへの CONNECT を頼む・
//! `sandbox_client::egress_proxy`）。環境の `HTTP_PROXY`・`HTTPS_PROXY` は、これらの送信には使わない（黙って拾わない）。
//!
//! 中継の認証のパスワードは、設定や環境変数に書かずにファイルで渡す（`proxy_password_file`）。中継の URL に利用者名や
//! パスワードを書くと起動しない（ログ・設定の表示に残さない）。

use serde::{Deserialize, Serialize};

use sandbox_client::egress_proxy::EgressProxy;

use super::ConfigError;

/// 外への送信の中継の設定。既定は中継を使わない。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EgressConfig {
    /// 中継の URL（`http://host:port`。利用者名・パスワード・パスは書かない）。空・未設定なら中継を使わない。
    #[serde(default)]
    pub proxy_url: Option<String>,
    /// 中継の Basic 認証の利用者名（`proxy_password_file` と両方書く）。
    #[serde(default)]
    pub proxy_username: Option<String>,
    /// 中継の Basic 認証のパスワードを書いたファイルのパス（末尾の改行は外して読む）。
    #[serde(default)]
    pub proxy_password_file: Option<String>,
}

/// compose は未設定の変数を空文字で渡すので、空（空白だけ）は書いていないものとして扱う。
fn non_empty(v: Option<&String>) -> Option<&str> {
    v.map(|s| s.trim()).filter(|s| !s.is_empty())
}

impl EgressConfig {
    /// 起動時の確かめ（パスワードのファイルは読まない）。
    pub(crate) fn check(&self) -> Result<(), ConfigError> {
        let user = non_empty(self.proxy_username.as_ref());
        let file = non_empty(self.proxy_password_file.as_ref());
        let Some(url) = non_empty(self.proxy_url.as_ref()) else {
            if user.is_some() || file.is_some() {
                return Err(ConfigError::Invalid(
                    "egress.proxy_username と egress.proxy_password_file は、egress.proxy_url を書いたときだけ\
                     使います。"
                        .into(),
                ));
            }
            return Ok(());
        };
        if user.is_some() != file.is_some() {
            return Err(ConfigError::Invalid(
                "中継の認証には、egress.proxy_username と egress.proxy_password_file を両方書いてください。".into(),
            ));
        }
        // URL の形と利用者名を確かめる（パスワードは起動の配線で読む）。
        EgressProxy::new(url, user.map(|u| (u, "")))
            .map(|_| ())
            .map_err(ConfigError::Invalid)
    }

    /// 中継を組む（パスワードのファイルを読む）。中継を書いていなければ `None`。
    pub fn proxy(&self) -> Result<Option<EgressProxy>, ConfigError> {
        self.check()?;
        let Some(url) = non_empty(self.proxy_url.as_ref()) else {
            return Ok(None);
        };
        let credentials = match (
            non_empty(self.proxy_username.as_ref()),
            non_empty(self.proxy_password_file.as_ref()),
        ) {
            (Some(user), Some(path)) => {
                let raw = std::fs::read_to_string(path).map_err(|e| {
                    ConfigError::Invalid(format!(
                        "egress.proxy_password_file（{path}）を読めません: {}",
                        e.kind()
                    ))
                })?;
                Some((user, raw.trim_end_matches(['\r', '\n']).to_string()))
            }
            _ => None,
        };
        EgressProxy::new(
            url,
            credentials
                .as_ref()
                .map(|(user, password)| (*user, password.as_str())),
        )
        .map(Some)
        .map_err(ConfigError::Invalid)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn cfg(url: Option<&str>, user: Option<&str>, file: Option<&str>) -> EgressConfig {
        EgressConfig {
            proxy_url: url.map(str::to_string),
            proxy_username: user.map(str::to_string),
            proxy_password_file: file.map(str::to_string),
        }
    }

    fn invalid(r: Result<impl std::fmt::Debug, ConfigError>) -> String {
        match r.unwrap_err() {
            ConfigError::Invalid(m) => m,
            ConfigError::Load(e) => panic!("{e}"),
        }
    }

    /// 既定（と compose が渡す空文字）は中継を使わない。
    #[test]
    fn no_proxy_by_default() {
        assert!(EgressConfig::default().proxy().unwrap().is_none());
        assert!(cfg(Some(" "), Some(""), Some(""))
            .proxy()
            .unwrap()
            .is_none());
    }

    /// 中継の URL だけなら認証なし。認証は利用者名とパスワードのファイルを両方書き、ファイルは末尾の改行を外して読む。
    #[test]
    fn credentials_come_from_the_username_and_the_password_file() {
        let plain = cfg(Some("http://proxy.corp.example:8080"), None, None)
            .proxy()
            .unwrap()
            .unwrap();
        assert_eq!(
            format!("{plain:?}"),
            "EgressProxy { endpoint: \"proxy.corp.example:8080\", authenticated: false }"
        );

        let dir = std::env::temp_dir().join(format!("shiki-egress-cfg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let file = dir.join("proxy-password");
        std::fs::write(&file, "s3cret\n").unwrap();
        let path = file.to_str().unwrap();
        let authed = cfg(
            Some("http://proxy.corp.example:8080"),
            Some("shiki"),
            Some(path),
        )
        .proxy()
        .unwrap()
        .unwrap();
        assert!(format!("{authed:?}").contains("authenticated: true"));
        assert!(!format!("{authed:?}").contains("s3cret"));

        let missing = dir.join("missing");
        let err = invalid(
            cfg(
                Some("http://proxy.corp.example:8080"),
                Some("shiki"),
                Some(missing.to_str().unwrap()),
            )
            .proxy(),
        );
        assert!(err.contains("egress.proxy_password_file"), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 書き方の誤りは起動で止める（URL に資格情報を書いたときも、その URL を映し返さない）。
    #[test]
    fn mistakes_stop_the_start_without_echoing_the_url() {
        let err = invalid(cfg(Some("http://shiki:s3cret@proxy:8080"), None, None).check());
        assert!(err.contains("利用者名やパスワード"), "{err}");
        assert!(!err.contains("s3cret"));
        let err = invalid(cfg(Some("https://proxy:8443"), None, None).check());
        assert!(err.contains("http:// で始めて"), "{err}");
        let err = invalid(cfg(Some("http://proxy:8080"), Some("shiki"), None).check());
        assert!(err.contains("両方"), "{err}");
        let err = invalid(cfg(None, Some("shiki"), Some("/run/secrets/p")).check());
        assert!(err.contains("egress.proxy_url を書いたときだけ"), "{err}");
    }

    /// 起動の確かめ（`AppConfig::validate`）が、中継の書き方と、ミニアプリの関数の内部の送り先を確かめる。
    #[test]
    fn the_start_checks_the_proxy_and_the_function_internal_hosts() {
        let mut cfg = super::super::tests::valid_config();
        cfg.egress.proxy_url = Some("http://shiki:s3cret@proxy:8080".into());
        let err = cfg.validate().unwrap_err().to_string();
        assert!(
            err.contains("利用者名やパスワード") && !err.contains("s3cret"),
            "{err}"
        );
        cfg.egress.proxy_url = Some("http://proxy:8080".into());
        cfg.gateway.function_http_internal_hosts = vec!["erp.corp.example:8443".into()];
        assert!(cfg.validate().is_ok());
        cfg.gateway
            .function_http_internal_hosts
            .push("*.corp.example".into());
        let err = cfg.validate().unwrap_err().to_string();
        assert!(
            err.contains("gateway.function_http_internal_hosts の「*.corp.example」を読めません。"),
            "{err}"
        );
    }
}
