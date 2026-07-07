//! `web_fetch` ツール（Phase 4 web ツール・sandbox egress の実消費者）。
//!
//! 入力 URL のホストだけを **当該 run 限定の dynamic_allow** に載せた短命サンドボックスを立て、
//! guest の Python（urllib）でページを取得する（design §4.4）。セキュリティ境界:
//! - **リダイレクト非追従**（PIT-36）: allowlist 外ホストへの誘導を遮断。
//! - **シークレット添付不可**: spec の `secret_attach=false` 固定。
//! - **内部ホスト拒否**（SSRF）: IP リテラル・単一ラベル名・localhost/.local/.internal 等を
//!   クライアント側で拒否し、egress allowlist（kernel default-deny）と二重防御にする。
//! - 宛先は spec（egress ポリシ全文）として orchestrator 側で監査記録される。

use std::sync::Arc;

use authz::AuthContext;
use sandbox_client::{ExecRequest, Sandbox, SandboxSpec};
use url::{Host, Url};

use super::sandbox_exec::{collect_output, truncate};
use crate::tool::{Tool, ToolError, ToolOutcome};

/// 取得本文の guest 側読み取り上限（モデル向け整形上限は別途 truncate が掛かる）。
const FETCH_BODY_CAP: usize = 256 * 1024;

/// `web_fetch` ツール。サンドボックスに `Sandbox` トレイト裏でアクセスする。
pub struct WebFetchTool {
    sandbox: Arc<dyn Sandbox>,
}

impl WebFetchTool {
    pub fn new(sandbox: Arc<dyn Sandbox>) -> Self {
        WebFetchTool { sandbox }
    }
}

/// 検証済みの取得先（egress allowlist へ載せる host/port と正規化済み URL）。
struct FetchTarget {
    url: Url,
    host: String,
    port: u16,
}

/// 入力 URL を検証する（モデル/ユーザー由来＝敵対的として扱う）。
///
/// - スキームは http/https のみ（gopher/file 等を拒否）。
/// - userinfo（`user:pass@`）付きは拒否（ホスト偽装・資格情報混入の防止）。
/// - ホストは **ドットを含む公開 FQDN のみ**: IP リテラル（v4/v6）・単一ラベル名
///   （compose のサービス名 `minio` 等）・localhost/.local/.internal/.lan/.home.arpa を拒否。
///   DNS 解決後の宛先制御は sandbox egress（default-deny＋当該ホストのみ allow）が担う。
fn validate_url(input: &str) -> Result<FetchTarget, ToolError> {
    let invalid = |msg: &str| ToolError::Invalid(format!("URL が不正です: {msg}"));
    let url = Url::parse(input.trim()).map_err(|e| invalid(&e.to_string()))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(invalid("http/https のみ取得できます"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(invalid("userinfo（user:pass@）付き URL は使えません"));
    }
    let host = match url.host() {
        Some(Host::Domain(d)) => d.to_ascii_lowercase(),
        Some(Host::Ipv4(_) | Host::Ipv6(_)) => {
            return Err(invalid("IP アドレス直指定は使えません"));
        }
        None => return Err(invalid("ホストがありません")),
    };
    // 内部/ローカル名を拒否（SSRF・confused-deputy の素地を断つ）。
    let forbidden_suffixes = [".local", ".internal", ".localhost", ".lan", ".home.arpa"];
    if !host.contains('.')
        || host == "localhost"
        || forbidden_suffixes.iter().any(|s| host.ends_with(s))
    {
        return Err(invalid("内部/ローカルホストは取得できません"));
    }
    let port = url
        .port_or_known_default()
        .ok_or_else(|| invalid("ポートを特定できません"))?;
    Ok(FetchTarget { url, host, port })
}

/// guest で実行する Python 取得コード（リダイレクト非追従・読み取り上限つき）。
fn fetch_code(url: &Url) -> String {
    // URL は JSON 文字列リテラルとして埋める（JSON のエスケープは Python 文字列と互換）。
    let url_literal = serde_json::Value::String(url.to_string()).to_string();
    format!(
        r#"import urllib.request, urllib.error, sys

class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None  # リダイレクトは追従しない（PIT-36）

opener = urllib.request.build_opener(NoRedirect)
req = urllib.request.Request({url_literal}, headers={{"User-Agent": "shiki-web-fetch/1.0"}})
try:
    resp = opener.open(req, timeout=20)
    print("HTTP", resp.status)
    print(resp.read({FETCH_BODY_CAP}).decode("utf-8", "replace"))
except urllib.error.HTTPError as e:
    # 3xx（非追従）・4xx・5xx はステータスと本文冒頭を観測として返す。
    print("HTTP", e.code)
    location = e.headers.get("Location")
    if location:
        print("Location:", location)
    print(e.read(65536).decode("utf-8", "replace"))
except Exception as e:
    print("fetch error:", e, file=sys.stderr)
    sys.exit(1)
"#
    )
}

#[async_trait::async_trait]
impl Tool for WebFetchTool {
    #[allow(clippy::unnecessary_literal_bound)]
    fn name(&self) -> &str {
        "web_fetch"
    }

    #[allow(clippy::unnecessary_literal_bound)]
    fn description(&self) -> &str {
        "URL のページを取得して本文を返す（リダイレクトは追従しない）。web_search で得た URL の\
         内容を読むときに使う。取得は隔離サンドボックス経由で、その URL のホストにしか通信できない。"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "取得する URL（http/https）" }
            },
            "required": ["url"],
            "additionalProperties": false
        })
    }

    // 読み取りのみ・シークレット非添付・宛先は run 限定 allowlist。確認不要。
    fn requires_confirmation(&self) -> bool {
        false
    }

    async fn call(
        &self,
        ctx: &AuthContext,
        input: serde_json::Value,
        _trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let url_input = input
            .get("url")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ToolError::Invalid("missing 'url'".into()))?;
        let target = validate_url(url_input)?;

        let spec = SandboxSpec::web_fetch(
            ctx.tenant_id.clone(),
            ctx.org.clone(),
            ctx.principal.id.clone(),
            target.host.clone(),
            target.port,
        );
        let handle = self
            .sandbox
            .create(spec)
            .await
            .map_err(|e| ToolError::Unavailable(format!("sandbox create: {e}")))?;

        let exec_result = self
            .sandbox
            .exec(
                &handle,
                ExecRequest::Python {
                    code: fetch_code(&target.url),
                    timeout_ms: None,
                },
            )
            .await;
        // `?` で早期 return すると destroy がスキップされるため、必ず destroy を通す形にする。
        let outcome = match exec_result {
            Ok(stream) => collect_output(stream)
                .await
                .map(|(stdout, stderr, exit, limit)| match (exit, limit) {
                    (_, Some(l)) => ToolOutcome::error(format!("{}\n{l}", truncate(&stdout))),
                    (Some(0), None) => ToolOutcome::ok(truncate(&stdout)),
                    (_, None) => ToolOutcome::error(format!(
                        "取得に失敗しました（宛先が egress 許可外の可能性）:\n{}",
                        truncate(&stderr)
                    )),
                }),
            Err(e) => Err(ToolError::Unavailable(format!("sandbox exec: {e}"))),
        };
        let _ = self.sandbox.destroy(&handle).await;
        outcome
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use sandbox_client::{FakeExecResult, FakeSandbox};

    fn ctx() -> AuthContext {
        AuthContext::new(
            authz::Principal {
                kind: authz::PrincipalKind::User,
                id: "u1".into(),
                email: None,
                groups: vec![],
                roles: vec![],
                tenant_id: Some("t1".into()),
            },
            "org1".into(),
            "t1".into(),
        )
    }

    #[test]
    fn validate_url_accepts_public_fqdn() {
        let t = validate_url("https://example.com/page?q=1").unwrap();
        assert_eq!(t.host, "example.com");
        assert_eq!(t.port, 443);
        let t = validate_url("http://sub.example.co.jp:8080/").unwrap();
        assert_eq!(t.port, 8080);
    }

    #[test]
    fn validate_url_rejects_dangerous_inputs() {
        // スキーム・userinfo・IP リテラル・内部/ローカル名（SSRF/PIT-36 系）を全部弾く。
        for bad in [
            "file:///etc/passwd",
            "gopher://example.com/",
            "https://user:pass@example.com/",
            "http://127.0.0.1/",
            "http://[::1]/",
            "http://10.0.0.5/",
            "http://minio/", // 単一ラベル（compose サービス名）
            "http://localhost/",
            "http://foo.local/",
            "http://metadata.google.internal/computeMetadata/v1/",
            "http://router.lan/",
            "not a url",
        ] {
            assert!(validate_url(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn fetch_code_embeds_url_safely() {
        // 引用符を含む URL でも Python 文字列リテラルとして安全に埋まる（JSON エスケープ）。
        let t = validate_url("https://example.com/a?b=\"c\"").unwrap();
        let code = fetch_code(&t.url);
        assert!(code.contains(r#"https://example.com/a?b=%22c%22"#) || code.contains("\\\""));
        assert!(code.contains("NoRedirect"));
    }

    #[tokio::test]
    async fn creates_sandbox_with_run_scoped_egress() {
        let sandbox = Arc::new(
            FakeSandbox::new().with_exec(FakeExecResult::stdout("HTTP 200\n<html>ok</html>\n")),
        );
        let tool = WebFetchTool::new(sandbox.clone());
        let out = tool
            .call(
                &ctx(),
                serde_json::json!({"url": "https://example.com/"}),
                None,
            )
            .await
            .expect("ok");
        assert!(!out.is_error);
        assert!(out.content.contains("HTTP 200"));
        // 当該ホストのみが run 限定 dynamic_allow に載る（静的 allow は空・シークレット非添付）。
        let specs = sandbox.created_specs();
        assert_eq!(specs.len(), 1);
        assert!(specs[0].egress.static_allow.is_empty());
        assert_eq!(specs[0].egress.dynamic_allow.len(), 1);
        assert_eq!(specs[0].egress.dynamic_allow[0].host_pattern, "example.com");
        assert_eq!(specs[0].egress.dynamic_allow[0].port, 443);
        assert!(!specs[0].egress.secret_attach);
        // 実行後に破棄される。
        assert_eq!(sandbox.destroyed().len(), 1);
    }

    #[tokio::test]
    async fn nonzero_exit_is_error() {
        let sandbox = Arc::new(FakeSandbox::new().with_exec(FakeExecResult {
            stdout: Vec::new(),
            stderr: b"fetch error: denied".to_vec(),
            exit_code: 1,
            artifacts: Vec::new(),
        }));
        let tool = WebFetchTool::new(sandbox);
        let out = tool
            .call(
                &ctx(),
                serde_json::json!({"url": "https://example.com/"}),
                None,
            )
            .await
            .expect("ok");
        assert!(out.is_error);
        assert!(out.content.contains("denied"));
    }

    #[tokio::test]
    async fn invalid_url_never_creates_sandbox() {
        let sandbox = Arc::new(FakeSandbox::new());
        let tool = WebFetchTool::new(sandbox.clone());
        let err = tool
            .call(
                &ctx(),
                serde_json::json!({"url": "http://127.0.0.1/"}),
                None,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Invalid(_)));
        assert!(sandbox.created_specs().is_empty());
    }
}
