//! [`super`]（web_fetch・#348）の単体テスト。
//!
//! SSRF/DNS リバインディング防御は「拒否できること」と「検証済みアドレスへ固定できること」
//! の両方を見る。後者はループバックのスタブサーバへ **解決不能なホスト名**で到達させて示す。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;

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

/// 呼び出し回数を数える固定リゾルバ（DNS リバインディングの再現に使う）。
struct StubResolver {
    addrs: Vec<SocketAddr>,
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl HostResolver for StubResolver {
    async fn lookup(&self, _host: &str, _port: u16) -> Result<Vec<SocketAddr>, &'static str> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.addrs.clone())
    }
}

fn tool_with(addrs: Vec<SocketAddr>) -> (WebFetchTool, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let tool = WebFetchTool::with_resolver(Arc::new(StubResolver {
        addrs,
        calls: calls.clone(),
    }));
    (tool, calls)
}

/// ループバックのスタブ HTTP サーバ。固定レスポンスを返し、受け取った Host ヘッダを晒す。
async fn stub_server(response: Vec<u8>) -> (SocketAddr, Arc<tokio::sync::Mutex<String>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(tokio::sync::Mutex::new(String::new()));
    let seen_w = seen.clone();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let mut buf = vec![0u8; 4096];
            let n = sock.read(&mut buf).await.unwrap_or(0);
            *seen_w.lock().await = String::from_utf8_lossy(&buf[..n]).into_owned();
            let _ = sock.write_all(&response).await;
            let _ = sock.flush().await;
        }
    });
    (addr, seen)
}

fn http_response(head: &str, body: &str) -> Vec<u8> {
    format!(
        "{head}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
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
fn textual_content_types_only() {
    for ok in [
        None,
        Some("text/html; charset=utf-8"),
        Some("application/json"),
        Some("application/ld+json"),
        Some("image/svg+xml"),
    ] {
        assert!(is_textual(ok), "{ok:?} は本文として読む");
    }
    for bad in [
        Some("image/png"),
        Some("application/pdf"),
        Some("application/octet-stream"),
        Some("video/mp4"),
    ] {
        assert!(!is_textual(bad), "{bad:?} は拒否");
    }
}

/// DNS リバインディング: 公開 IP に混ざった内部 IP を返す DNS 応答は**丸ごと**拒否する。
#[tokio::test]
async fn rejects_rebinding_to_internal_ip() {
    for addrs in [
        vec!["127.0.0.1:443".parse().unwrap()],
        vec!["169.254.169.254:80".parse().unwrap()], // クラウドメタデータ
        vec![
            "93.184.216.34:443".parse().unwrap(), // 公開に見せかけて…
            "10.0.0.5:443".parse().unwrap(),      // …内部を混ぜる
        ],
    ] {
        let (tool, _) = tool_with(addrs);
        let err = tool
            .call(
                &ctx(),
                serde_json::json!({"url": "https://example.com/"}),
                None,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Invalid(_)), "{err:?}");
    }
}

/// 検証済みアドレスへ**接続が固定**される（＝接続時に名前解決し直さない）。
///
/// URL のホストは RFC 2606 の `.invalid` で **解決不能**。それでも取得できるなら、
/// 接続先が DNS ではなく検証済みアドレス由来だと言える（DNS リバインディングの遮断）。
#[tokio::test]
async fn pins_connection_to_validated_address() {
    let (addr, seen) = stub_server(http_response(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8",
        "<html>pinned ok</html>",
    ))
    .await;
    let (mut tool, calls) = tool_with(vec![addr]);
    tool.skip_addr_guard = true; // ループバックのスタブへ繋ぐためだけの緩和（テスト専用）
    let out = tool
        .call(
            &ctx(),
            serde_json::json!({"url": format!("http://pinned.example.invalid:{}/", addr.port())}),
            None,
        )
        .await
        .unwrap();
    assert!(!out.is_error, "{}", out.content);
    assert!(out.content.contains("HTTP 200"));
    assert!(out.content.contains("pinned ok"));
    assert_eq!(calls.load(Ordering::SeqCst), 1, "解決は 1 回だけ");
    // Host ヘッダは URL のホストのまま（宛先だけを固定し、リクエストは偽装しない）。
    assert!(
        seen.lock().await.contains("pinned.example.invalid"),
        "Host ヘッダが書き換わっている"
    );
}

/// リダイレクトは追従せず、Location を観測として返す（PIT-36）。
#[tokio::test]
async fn does_not_follow_redirects() {
    let (addr, _) = stub_server(http_response(
        "HTTP/1.1 302 Found\r\nLocation: http://169.254.169.254/latest/meta-data/",
        "",
    ))
    .await;
    let (mut tool, _) = tool_with(vec![addr]);
    tool.skip_addr_guard = true;
    let out = tool
        .call(
            &ctx(),
            serde_json::json!({"url": format!("http://redir.example.invalid:{}/", addr.port())}),
            None,
        )
        .await
        .unwrap();
    assert!(out.content.contains("HTTP 302"));
    assert!(out.content.contains("169.254.169.254")); // 追従せず提示するだけ
    assert!(out.content.contains("追従しません"));
}

/// 200 応答に付いた `Location` はリダイレクト扱いしない（本文を捨てない）。
#[tokio::test]
async fn location_on_non_redirect_is_not_treated_as_redirect() {
    let (addr, _) = stub_server(http_response(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nLocation: /elsewhere",
        "<html>本文はここ</html>",
    ))
    .await;
    let (mut tool, _) = tool_with(vec![addr]);
    tool.skip_addr_guard = true;
    let out = tool
        .call(
            &ctx(),
            serde_json::json!({"url": format!("http://loc200.example.invalid:{}/", addr.port())}),
            None,
        )
        .await
        .unwrap();
    assert!(!out.is_error, "{}", out.content);
    assert!(out.content.contains("本文はここ"), "{}", out.content);
    assert!(!out.content.contains("追従しません"));
}

/// 巨大な本文は 256KiB で打ち切る（読み切らない）。
#[tokio::test]
async fn caps_body_size() {
    let body = "a".repeat(FETCH_BODY_CAP + 50_000);
    let (addr, _) = stub_server(http_response(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain",
        &body,
    ))
    .await;
    let (mut tool, _) = tool_with(vec![addr]);
    tool.skip_addr_guard = true;
    let out = tool
        .call(
            &ctx(),
            serde_json::json!({"url": format!("http://big.example.invalid:{}/", addr.port())}),
            None,
        )
        .await
        .unwrap();
    assert!(out.content.contains("256KiB で打ち切り"), "{}", out.content);
}

/// バイナリは本文を返さない（モデルへ渡す意味がなく、トークンを焼くだけ）。
#[tokio::test]
async fn rejects_binary_content() {
    let (addr, _) = stub_server(http_response(
        "HTTP/1.1 200 OK\r\nContent-Type: image/png",
        "\u{0089}PNG",
    ))
    .await;
    let (mut tool, _) = tool_with(vec![addr]);
    tool.skip_addr_guard = true;
    let out = tool
        .call(
            &ctx(),
            serde_json::json!({"url": format!("http://img.example.invalid:{}/", addr.port())}),
            None,
        )
        .await
        .unwrap();
    assert!(out.is_error);
    assert!(out.content.contains("テキストではない"));
}

/// 不正 URL は名前解決すらしない（DNS へ攻撃者の任意文字列を投げない）。
#[tokio::test]
async fn invalid_url_never_resolves() {
    let (tool, calls) = tool_with(vec!["93.184.216.34:80".parse().unwrap()]);
    let err = tool
        .call(
            &ctx(),
            serde_json::json!({"url": "http://127.0.0.1/"}),
            None,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Invalid(_)));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
