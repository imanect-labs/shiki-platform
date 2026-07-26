//! `web_fetch` の実ネットワーク結合テスト（`WEB_FETCH_LIVE=1` の時だけ実行・issue #348）。
//!
//! 単体テストは名前解決をスタブに差し替えているため、**実 DNS → 公開 IP 検証 → アドレス固定 →
//! TLS 接続**の一本道はここでしか通らない。CI 既定では走らせない（外部依存・ネットワーク必須）。
//!
//! 実行: `WEB_FETCH_LIVE=1 cargo test -p shiki-agent-core --test web_fetch_live -- --nocapture`

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]

use agent_core::{Tool, ToolError, WebFetchTool};
use authz::{AuthContext, Principal, PrincipalKind};

fn ctx() -> AuthContext {
    AuthContext::new(
        Principal {
            kind: PrincipalKind::User,
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

fn gated() -> bool {
    std::env::var("WEB_FETCH_LIVE").as_deref() == Ok("1")
}

/// 実在の公開サイトを HTTPS で取得できる（解決 → 公開 IP 検証 → 固定 → TLS）。
#[tokio::test]
async fn fetches_public_https_site() {
    if !gated() {
        return;
    }
    let out = WebFetchTool::new()
        .call(
            &ctx(),
            serde_json::json!({"url": "https://example.com/"}),
            None,
        )
        .await
        .expect("tool error");
    println!("--- web_fetch(example.com) ---\n{}", out.content);
    assert!(!out.is_error, "{}", out.content);
    assert!(out.content.contains("HTTP 200"));
    assert!(out.content.contains("Example Domain"));
}

/// 内部 IP へ解決される公開っぽい名前は拒否する（実 DNS で解決させたうえで弾く）。
///
/// `localtest.me` は 127.0.0.1 を返す実在のドメイン（TLD 付きなので URL 検証は通り、
/// **解決後 IP の再検証**だけが防波堤になる）。
#[tokio::test]
async fn rejects_public_name_resolving_to_loopback() {
    if !gated() {
        return;
    }
    let err = WebFetchTool::new()
        .call(
            &ctx(),
            serde_json::json!({"url": "http://localtest.me/"}),
            None,
        )
        .await
        .unwrap_err();
    println!("--- web_fetch(localtest.me) rejected: {err} ---");
    assert!(matches!(err, ToolError::Invalid(_)), "{err:?}");
}
