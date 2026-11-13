#![allow(clippy::unwrap_used, clippy::pedantic)]
use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
fn config() -> GitHubConfig {
    GitHubConfig {
        app_id: 1,
        installation_id: 2,
        // 鍵素材を新たにコミットしないよう、既存のテスト専用鍵（app-gateway）を流用する。
        private_key_file: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../app-gateway/tests/fixtures/test_rsa_priv.pem"),
        owner: "imanect-labs".into(),
        repo: "shiki-test".into(),
        profile: "preview".into(),
        config_version: 1,
    }
}
fn identity() -> DeliveryIdentity {
    DeliveryIdentity {
        id: uuid::Uuid::new_v4(),
        report_id: uuid::Uuid::new_v4(),
        submission_rev: 1,
    }
}
fn report() -> ConfirmedBugReport {
    ConfirmedBugReport {
        content: crate::ReportContent {
            category: crate::Category::Bug,
            title: "@alice example.com #123".into(),
            symptom: "症状\n@all".into(),
            steps_tried: "再読込".into(),
            expected_behavior: "期待".into(),
        },
        payload_hash: "hash".into(),
    }
}
pub(crate) async fn stub(
    responses: Vec<String>,
) -> (GitHubIssueSink, tokio::sync::mpsc::Receiver<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = tokio::sync::mpsc::channel(16);
    tokio::spawn(async move {
        for response in responses {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut chunk = [0; 8192];
                let n = stream.read(&mut chunk).await.unwrap();
                request.extend_from_slice(&chunk[..n]);
                let s = String::from_utf8_lossy(&request);
                if let Some((head, body)) = s.split_once("\r\n\r\n") {
                    let len = head
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .and_then(|v| v.parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if body.len() >= len {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            let _received = tx.send(String::from_utf8(request).unwrap()).await;
            if response == "timeout" {
                tokio::time::sleep(Duration::from_secs(16)).await;
            } else {
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        }
    });
    let mut sink = GitHubIssueSink::new(config()).unwrap();
    sink.stub = Some(format!("http://{addr}"));
    *sink.token.lock().await = Some(Token {
        value: "test-token".into(),
        expires: Utc::now() + chrono::Duration::hours(1),
    });
    (sink, rx)
}
pub(crate) fn response(code: u16, headers: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {code} Status\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
        body.len()
    )
}
pub(crate) fn issue(id: u64, body: &str) -> String {
    serde_json::json!({"number":id,"html_url":format!("https://github.com/imanect-labs/shiki-test/issues/{id}"),"body":body}).to_string()
}
#[test]
fn config_and_literal_boundaries() {
    let mut c = config();
    assert!(c.validate().is_ok());
    for repo in [
        "shiki-platform",
        "shiki-test/../shiki-platform",
        "shiki-test.evil",
    ] {
        c.repo = repo.into();
        assert!(c.validate().is_err());
    }
    c = config();
    c.owner = "evil".into();
    assert!(c.validate().is_err());
    c = config();
    c.profile = "production".into();
    assert!(c.validate().is_err());
    c = config();
    c.private_key_file = "relative".into();
    assert!(c.validate().is_err());
    assert!(crate::BugReportConfig {
        enabled: true,
        backend: crate::BugReportBackend::Github,
        github: None
    }
    .validate()
    .is_err());
    assert!(!neutralize("@a <b> ![x](https://x.y) #1 `")
        .contains(['@', '<', '>', '!', '[', ']', '.', '#', '`']));
    assert_eq!(literal("one\ntwo"), "    one\n    two");
}
#[tokio::test]
async fn success_and_fixed_payload() {
    let (sink, mut rx) = stub(vec![response(201, "", &issue(42, ""))]).await;
    assert!(
        matches!(sink.dispatch(&report(),&identity()).await,Dispatch::Sent { id,.. } if id=="42")
    );
    let request = rx.recv().await.unwrap();
    assert!(request.starts_with("POST /repos/imanect-labs/shiki-test/issues "));
    assert!(request.contains("authorization: Bearer test-token"));
    assert!(request.contains("x-github-api-version: 2022-11-28"));
    assert!(!request.contains("@alice"));
    assert!(!request.contains("tenant"));
    assert!(request.contains("## 種別\\n不具合"));
}
#[tokio::test]
async fn token_stage_failures_are_retryable_before_issue_post() {
    // token 取得のタイムアウトは Issue POST 前なので結果不明にしない。
    let (sink, mut rx) = stub(vec!["timeout".into()]).await;
    *sink.token.lock().await = None;
    assert_eq!(
        sink.dispatch(&report(), &identity()).await,
        Dispatch::Retry {
            after: 60,
            code: "credential_unavailable"
        }
    );
    assert!(rx
        .recv()
        .await
        .unwrap()
        .starts_with("POST /app/installations/2/access_tokens "));
    assert!(rx.try_recv().is_err());
    // token エンドポイントの rate limit は待機時間を尊重する。
    let (sink, _) = stub(vec![response(429, "Retry-After: 120\r\n", "")]).await;
    *sink.token.lock().await = None;
    assert_eq!(
        sink.dispatch(&report(), &identity()).await,
        Dispatch::Retry {
            after: 120,
            code: "rate_limit"
        }
    );
    // 資格情報の拒否は止める。
    let (sink, _) = stub(vec![response(401, "", "")]).await;
    *sink.token.lock().await = None;
    assert_eq!(
        sink.dispatch(&report(), &identity()).await,
        Dispatch::Failed("credential")
    );
}
#[test]
fn every_category_has_a_label() {
    assert_eq!(category_label(crate::Category::Bug), "不具合");
    assert_eq!(category_label(crate::Category::Usage), "使い方");
    assert_eq!(category_label(crate::Category::Other), "その他");
}
#[tokio::test]
async fn http_failures_and_retry() {
    for (code, header, expected) in [
        (401, "", Dispatch::Failed("unauthorized")),
        (403, "", Dispatch::Failed("forbidden")),
        (404, "", Dispatch::Failed("not_found")),
        (422, "", Dispatch::Failed("validation")),
        (
            429,
            "Retry-After: 700\r\n",
            Dispatch::Retry {
                after: 700,
                code: "rate_limit",
            },
        ),
        (
            403,
            "X-RateLimit-Remaining: 0\r\nRetry-After: 123\r\n",
            Dispatch::Retry {
                after: 123,
                code: "rate_limit",
            },
        ),
        (500, "", Dispatch::Unknown),
        (
            302,
            "Location: http://127.0.0.1:1/steal\r\n",
            Dispatch::Failed("redirect"),
        ),
    ] {
        let (sink, mut rx) =
            stub(vec![response(code, header, "secret error must not escape")]).await;
        assert_eq!(sink.dispatch(&report(), &identity()).await, expected);
        assert!(rx.recv().await.is_some());
    }
}
#[tokio::test]
async fn response_url_is_exact_and_unknown_never_posts() {
    for url in [
        "https://github.com.evil/imanect-labs/shiki-test/issues/1",
        "https://github.com/imanect-labs/shiki-platform/issues/1",
        "http://github.com/imanect-labs/shiki-test/issues/1",
        "https://github.com/imanect-labs/shiki-test/issues/1?x",
    ] {
        let body = serde_json::json!({"number":1,"html_url":url}).to_string();
        let (sink, _) = stub(vec![response(201, "", &body)]).await;
        assert_eq!(
            sink.dispatch(&report(), &identity()).await,
            Dispatch::Unknown
        );
    }
    let id = identity();
    let (sink, mut rx) = stub(vec![response(
        200,
        "",
        &format!("[{}]", issue(7, &marker(&id))),
    )])
    .await;
    assert!(matches!(sink.lookup(&id).await,Dispatch::Sent { id,.. } if id=="7"));
    assert!(rx.recv().await.unwrap().starts_with("GET "));
    let (sink, mut rx) = stub(vec![response(200, "", "[]")]).await;
    assert_eq!(sink.lookup(&id).await, Dispatch::Unknown);
    assert!(rx.recv().await.unwrap().starts_with("GET "));
}
#[tokio::test]
async fn timeout_and_response_limit() {
    let (sink, _) = stub(vec!["timeout".into()]).await;
    assert_eq!(
        sink.dispatch(&report(), &identity()).await,
        Dispatch::Unknown
    );
    let (sink, _) = stub(vec![response(201, "", &"x".repeat(1024 * 1024 + 1))]).await;
    assert_eq!(
        sink.dispatch(&report(), &identity()).await,
        Dispatch::Unknown
    );
}
#[tokio::test]
async fn jwt_exchange_and_memory_cache() {
    let token=serde_json::json!({"token":"short-token","expires_at":Utc::now()+chrono::Duration::hours(1)}).to_string();
    let (sink, mut rx) = stub(vec![
        response(201, "", &token),
        response(201, "", &issue(1, "")),
    ])
    .await;
    *sink.token.lock().await = None;
    assert!(matches!(
        sink.dispatch(&report(), &identity()).await,
        Dispatch::Sent { .. }
    ));
    assert_eq!(sink.token().await.unwrap(), "short-token");
    let request = rx.recv().await.unwrap();
    assert!(request.starts_with("POST /app/installations/2/access_tokens "));
    assert!(request.contains("authorization: Bearer ey"));
    assert!(rx.recv().await.unwrap().contains("Bearer short-token"));
}

#[tokio::test]
async fn redirect_never_leaks_authentication_and_connect_is_retryable() {
    let trap = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target = trap.local_addr().unwrap();
    let (sink, _requests) = stub(vec![response(
        307,
        &format!("Location: http://{target}/steal\r\n"),
        "",
    )])
    .await;
    assert_eq!(
        sink.dispatch(&report(), &identity()).await,
        Dispatch::Failed("redirect")
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), trap.accept())
            .await
            .is_err()
    );
    let (mut sink, _requests) = stub(vec![]).await;
    sink.stub = Some(format!("http://{target}"));
    drop(trap);
    assert_eq!(
        sink.dispatch(&report(), &identity()).await,
        Dispatch::Retry {
            after: 0,
            code: "connect"
        }
    );
    assert!(sandbox_client::net_guard::ensure_all_public(vec![target]).is_err());
}
#[tokio::test]
async fn sink_trait_adapts_results_without_credentials_in_errors() {
    let ctx = AuthContext::new(
        authz::Principal {
            kind: authz::PrincipalKind::User,
            id: "alice".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: None,
        },
        "org".into(),
        authz::TenantId::parse("test").unwrap(),
    );
    let (sink, _requests) = stub(vec![response(201, "", &issue(8, ""))]).await;
    assert_eq!(
        sink.send(&ctx, &report(), &identity()).await,
        SendOutcome::Sent {
            external_id: "8".into()
        }
    );
    let (sink, _requests) = stub(vec![response(401, "", "secret-token")]).await;
    assert_eq!(
        sink.send(&ctx, &report(), &identity()).await,
        SendOutcome::Failed
    );
    let (sink, _requests) = stub(vec![response(500, "", "secret-token")]).await;
    assert_eq!(
        sink.send(&ctx, &report(), &identity()).await,
        SendOutcome::Unknown
    );
    let id = identity();
    let (sink, _requests) = stub(vec![response(
        200,
        "",
        &format!("[{}]", issue(9, &marker(&id))),
    )])
    .await;
    assert_eq!(
        sink.reconcile(&ctx, &id).await,
        ReconcileOutcome::Found {
            external_id: "9".into()
        }
    );
    let (sink, _requests) = stub(vec![response(200, "", "[]")]).await;
    assert_eq!(
        sink.reconcile(&ctx, &id).await,
        ReconcileOutcome::Inconclusive
    );
}

#[tokio::test]
async fn token_server_errors_retry_without_posting_issue() {
    for code in [500, 502, 503] {
        let (sink, mut rx) = stub(vec![response(code, "", "private error")]).await;
        *sink.token.lock().await = None;
        assert_eq!(
            sink.dispatch(&report(), &identity()).await,
            Dispatch::Retry {
                after: 60,
                code: "credential_unavailable"
            }
        );
        assert!(rx
            .recv()
            .await
            .unwrap()
            .starts_with("POST /app/installations/2/access_tokens "));
        assert!(rx.try_recv().is_err());
    }
}
#[tokio::test]
async fn secondary_limits_and_primary_reset_respect_longer_wait() {
    for message in [
        "You have exceeded a secondary rate limit.",
        "Resource not accessible by integration",
    ] {
        let body = serde_json::json!({"message":message}).to_string();
        let (sink, _) = stub(vec![response(403, "", &body)]).await;
        let expected = if message.contains("secondary") {
            Dispatch::Retry {
                after: 60,
                code: "rate_limit",
            }
        } else {
            Dispatch::Failed("forbidden")
        };
        assert_eq!(sink.dispatch(&report(), &identity()).await, expected);
    }
    for (retry, reset) in [(30, 600), (900, 60)] {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("retry-after", retry.to_string().parse().unwrap());
        headers.insert("x-ratelimit-remaining", "0".parse().unwrap());
        headers.insert(
            "x-ratelimit-reset",
            (Utc::now().timestamp() + reset)
                .to_string()
                .parse()
                .unwrap(),
        );
        assert!(matches!(classify(403, &headers, b""),
            Dispatch::Retry { after, code: "rate_limit" }
            if after >= u64::try_from(retry.max(reset) - 1).unwrap()));
    }
    assert_eq!(
        classify(429, &reqwest::header::HeaderMap::new(), b""),
        Dispatch::Retry {
            after: 60,
            code: "rate_limit"
        }
    );
}

#[tokio::test]
async fn reconcile_rate_limit_is_shared_without_reposting() {
    let (sink, mut requests) = stub(vec![response(
        403,
        "",
        r#"{"message":"You have exceeded a secondary rate limit."}"#,
    )])
    .await;
    assert_eq!(
        sink.lookup(&identity()).await,
        Dispatch::Retry {
            after: 60,
            code: "rate_limit"
        }
    );
    assert!(requests.recv().await.unwrap().starts_with("GET "));
    assert!(requests.try_recv().is_err());
}
