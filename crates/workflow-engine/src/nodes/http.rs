//! `http.request` 能力ノード（外部 egress・Task 10.10・engine.md §9.5・PIT-36）。
//!
//! 宛先束縛（allowlist ∩ secret の allowed_hosts）を **URL ホスト部リテラル**で照合し、
//! リダイレクトは既定で拒否（302→内部 IP への rebind を封じる）。SSRF/DNS rebinding/近似ホスト
//! 攻撃を保存時（V4）＋実行時の二重で弾く。冪等キー（Idempotency-Key）を外部へ伝える。

use secrets::DestinationBinding;
use serde_json::{json, Value};

/// http.request のリクエスト仕様（IR params から組む）。
#[derive(Debug, Clone)]
pub struct HttpSpec {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
    /// この step の冪等キー（Idempotency-Key ヘッダに載せる）。
    pub idempotency_key: String,
}

/// http 実行の失敗理由。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpDenied {
    /// URL が解析不能。
    BadUrl,
    /// ホストが宛先束縛の外（allowlist ∩ secret.allowed_hosts）。
    HostNotAllowed(String),
    /// リダイレクト（既定で拒否）。
    RedirectDenied,
    /// スキーム不許可（http/https のみ）。
    BadScheme,
}

/// URL からホスト部を取り出し、宛先束縛に照合する（**リテラル照合**・DNS 解決前）。
///
/// リテラルのホスト名で照合するため DNS rebinding（解決結果すり替え）に影響されない。
/// スキームは http/https のみ許可。ホストが束縛外なら `Err(HostNotAllowed)`。
pub fn check_destination(url: &str, binding: &DestinationBinding) -> Result<String, HttpDenied> {
    let parsed = url::Url::parse(url).map_err(|_| HttpDenied::BadUrl)?;
    match parsed.scheme() {
        "http" | "https" => {}
        _ => return Err(HttpDenied::BadScheme),
    }
    let host = parsed
        .host_str()
        .ok_or(HttpDenied::BadUrl)?
        .to_ascii_lowercase();
    if binding.allows(&host) {
        Ok(host)
    } else {
        Err(HttpDenied::HostNotAllowed(host))
    }
}

/// レスポンス要約（run 履歴・effect journal に載せる・**本文は redact**）。
#[must_use]
pub fn summarize_response(status: u16, host: &str) -> Value {
    // 本文・ヘッダ（Authorization 等）は載せない。ステータスと宛先ホストのみ。
    json!({ "status": status, "host": host })
}

/// リダイレクト方針: 既定は拒否（engine.md §9.5）。`follow` が真でも allowlist 内のみ許可する
/// 前提だが Stage A は一律拒否で安全側に倒す。
#[must_use]
pub fn redirect_denied(status: u16) -> bool {
    (300..400).contains(&status)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(hosts: &[&str]) -> DestinationBinding {
        DestinationBinding::new(hosts.iter().map(|s| (*s).to_string()).collect())
    }

    #[test]
    fn allows_exact_host() {
        let b = binding(&["api.slack.com"]);
        assert_eq!(
            check_destination("https://api.slack.com/x", &b),
            Ok("api.slack.com".into())
        );
    }

    #[test]
    fn rejects_near_host_and_subdomain_attack() {
        let b = binding(&["api.slack.com"]);
        // 近似ホスト（サフィックス付加）。
        assert_eq!(
            check_destination("https://api.slack.com.evil.com/x", &b),
            Err(HttpDenied::HostNotAllowed("api.slack.com.evil.com".into()))
        );
        // 上位ドメイン。
        assert_eq!(
            check_destination("https://slack.com/x", &b),
            Err(HttpDenied::HostNotAllowed("slack.com".into()))
        );
    }

    #[test]
    fn rejects_non_http_scheme() {
        let b = binding(&["api.slack.com"]);
        // file:// や internal metadata への SSRF を封じる。
        assert_eq!(
            check_destination("file:///etc/passwd", &b),
            Err(HttpDenied::BadScheme)
        );
        assert_eq!(
            check_destination("gopher://api.slack.com/", &b),
            Err(HttpDenied::BadScheme)
        );
    }

    #[test]
    fn case_insensitive_host() {
        let b = binding(&["api.slack.com"]);
        assert_eq!(
            check_destination("https://API.SLACK.COM/x", &b),
            Ok("api.slack.com".into())
        );
    }

    #[test]
    fn wildcard_binding() {
        let b = binding(&["*.slack.com"]);
        assert!(check_destination("https://hooks.slack.com/x", &b).is_ok());
        assert_eq!(
            check_destination("https://slack.com.evil.com/x", &b),
            Err(HttpDenied::HostNotAllowed("slack.com.evil.com".into()))
        );
    }

    #[test]
    fn redirect_is_denied() {
        assert!(redirect_denied(302));
        assert!(redirect_denied(301));
        assert!(!redirect_denied(200));
    }

    #[test]
    fn summary_omits_body_and_auth() {
        let s = summarize_response(200, "api.slack.com");
        assert_eq!(s, json!({ "status": 200, "host": "api.slack.com" }));
        // 本文やヘッダのキーが無いこと。
        assert!(s.get("body").is_none());
        assert!(s.get("headers").is_none());
    }
}
