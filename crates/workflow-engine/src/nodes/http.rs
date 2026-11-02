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

/// 相手が誤り（400〜499。429 を除く）を返したときの code（`fail_on_error_status`・permanent・BUG-99・D-95）。
///
/// 送る内容・URL・シークレットを直すまで通らないので、やり直さない（失敗の分類は設定の誤り）。
pub const HTTP_ERROR_STATUS: &str = "http_error_status";

/// 相手が一時的な誤り（500 以上と 408・425・429）を返したときの code（`fail_on_error_status`・retryable・BUG-99・D-95）。
///
/// 試す回数が 2 以上ならやり直す（失敗の分類は一時障害）。408（Request Timeout）と 425（Too Early）は、同じ要求を
/// 後で送り直せば通りうる（送る内容を直す誤りではない）。並行上限の順番待ちの `rate_limited` とは分ける
/// （あちらは試す回数を消費しないので、相手の 429 に使うと何度でも送り直してしまう）。
pub const HTTP_RETRYABLE_STATUS: &str = "http_retryable_status";

/// 送り先の名前が内部のネットワークの IP にもつながるので送らなかったときの code（ポートの送り口が名前解決の後に
/// 断る・permanent・BUG-181・D-135）。
///
/// 管理者がデプロイ設定 `workflow.http_internal_hosts`（内部の送り先・送れる先の許可 `http_allowlist` とは別）に
/// そのホストを書いたときだけ、内部の IP へ送れる。
pub const DESTINATION_INTERNAL: &str = "destination_internal";

/// 送り先の名前がリンクローカル・クラウドのメタデータの IP につながるので送らなかったときの code（管理者の許可が
/// あっても断る・permanent・BUG-181・D-135）。
pub const DESTINATION_LINK_LOCAL: &str = "destination_link_local";

/// 送り先の名前のアドレスが無い（NXDOMAIN・名前はあるがアドレスが無い）ので送れなかったときの code（URL のドメインの
/// 誤り・permanent・BUG-202）。
///
/// DNS に届かない・一時的に引けないとき（やり直せる `unavailable`）とは分ける。見分けられないときは `unavailable` のまま
/// （`sandbox_client::net_guard::NO_SUCH_HOST`）。試す回数を増やしたブロックでも、URL の誤りではやり直さない。
pub const DESTINATION_NOT_FOUND: &str = "destination_not_found";

/// 送り先のアドレスで断った失敗か（エンジンが監査に残す）。
#[must_use]
pub fn destination_refused(code: &str) -> bool {
    code == DESTINATION_INTERNAL || code == DESTINATION_LINK_LOCAL
}

/// 応答のステータスコードを失敗に分ける（`fail_on_error_status` のときだけ使う）。成功（399 以下）は `None`、
/// 失敗は（code・やり直せるか）。
#[must_use]
pub fn classify_error_status(status: u16) -> Option<(&'static str, bool)> {
    match status {
        0..=399 => None,
        408 | 425 | 429 | 500.. => Some((HTTP_RETRYABLE_STATUS, true)),
        _ => Some((HTTP_ERROR_STATUS, false)),
    }
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

    /// 4xx はやり直さない失敗、5xx と 408・425・429 はやり直せる失敗、399 以下は成功（BUG-99）。
    #[test]
    fn error_statuses_are_classified() {
        for s in [200, 204, 304, 399] {
            assert_eq!(classify_error_status(s), None, "{s}");
        }
        for s in [400, 401, 403, 404, 409, 422, 499] {
            assert_eq!(
                classify_error_status(s),
                Some((HTTP_ERROR_STATUS, false)),
                "{s}"
            );
        }
        for s in [408, 425, 429, 500, 502, 503, 599] {
            assert_eq!(
                classify_error_status(s),
                Some((HTTP_RETRYABLE_STATUS, true)),
                "{s}"
            );
        }
    }

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
