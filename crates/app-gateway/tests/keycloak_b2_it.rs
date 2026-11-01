//! 実 Keycloak で、B2（サーバーの関数）のトークンを確かめる（BUG-182・183）。
//!
//! `KEYCLOAK_TEST_URL`（realm `shiki` を import 済みの Keycloak・例 `http://localhost:8081`）が無ければスキップ。
//! 作ったクライアントはテストの最後に消す。モックの Keycloak（`keycloak_mock.rs`）は何でも通すので、Keycloak 26.2 の
//! 標準のトークンの交換の決まり（交換を頼むクライアントが元のトークンの aud に入っていること・audience はクライアント
//! の id だけ・scope は足すだけ）は、ここで本物を相手に確かめる。
//!
//! - 利用者が起点の呼び出し: 同じアプリの B2 が、B1 で取った利用者のトークンを交換できる。sub は利用者のまま、能力
//!   スコープは元のトークンより広がらない。別のアプリの B2 は交換できない。交換の相手を aud に足していない前の版の
//!   B1 は、揃えると交換できるようになる。アンインストール（B1 を無効）の後は、発行済みのトークンも交換できない。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::large_futures
)]

use std::panic::AssertUnwindSafe;

use app_gateway::test_support::{claims, TestRealm};
use app_gateway::{
    exchange_for_user, verify_gateway_token, ClientKind, GatewayError, GatewayTokenConfig,
};
use authz::CapabilityScope;
use futures::FutureExt as _;
use uuid::Uuid;

/// テストの本体を走らせ、落ちても片付け（`cleanup`）をしてから落とす。
async fn run_then<F, C>(body: F, cleanup: C)
where
    F: std::future::Future<Output = ()>,
    C: std::future::Future<Output = ()>,
{
    let outcome = AssertUnwindSafe(body).catch_unwind().await;
    cleanup.await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

/// web シェルのホスト支援 PKCE の戻り先（web/src/lib/miniapp-oauth.ts と同じ）。
const SHELL_CALLBACK: &str = "http://localhost:3000/apps/oauth/callback";
/// realm の試しの利用者の sub（deploy/keycloak/shiki-realm.json）。
const ALICE: &str = "00000000-0000-0000-0000-000000000001";

fn gateway_cfg(realm: &TestRealm) -> GatewayTokenConfig {
    GatewayTokenConfig {
        audience: "shiki-gateway".into(),
        issuer: realm.issuer(),
    }
}

/// トークンの aud（1 つでも配列でも）。
fn audiences(token: &str) -> Vec<String> {
    match &claims(token)["aud"] {
        serde_json::Value::String(s) => vec![s.clone()],
        serde_json::Value::Array(a) => a
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        _ => vec![],
    }
}

#[tokio::test]
async fn b2_exchanges_its_own_b1_users_tokens_without_widening() {
    let Some(realm) = TestRealm::from_env() else {
        eprintln!("KEYCLOAK_TEST_URL 未設定のためスキップ");
        return;
    };
    let app = Uuid::new_v4();
    let (b1, b2) = (format!("app-{app}-b1"), format!("app-{app}-b2"));
    let other_b2 = format!("app-{}-b2", Uuid::new_v4());
    let oauth = realm.oauth_client();
    let http = reqwest::Client::new();
    let redirects = [SHELL_CALLBACK.to_string()];
    let body = async {
        oauth
            .register(ClientKind::PublicPkce, &b1, "経費", &redirects, Some(&b2))
            .await
            .expect("register b1");
        let secret = oauth
            .register(ClientKind::Confidential, &b2, "経費", &[], None)
            .await
            .expect("register b2")
            .client_secret
            .expect("b2 secret");
        let other_secret = oauth
            .register(ClientKind::Confidential, &other_b2, "別のアプリ", &[], None)
            .await
            .expect("register other b2")
            .client_secret
            .expect("other secret");

        // B1 のトークンの aud には、ゲートウェイと同じアプリの B2 が入る。
        let token = realm
            .user_token(&b1, SHELL_CALLBACK, "alice", "openid data.read")
            .await
            .expect("token");
        let aud = audiences(&token);
        assert!(aud.contains(&"shiki-gateway".to_string()), "{aud:?}");
        assert!(aud.contains(&b2), "{aud:?}");
        assert!(!aud.contains(&other_b2), "{aud:?}");

        // 同じアプリの B2 は交換できる。sub は利用者のまま、能力スコープは元のトークンのものだけ（B2 は能力スコープを
        // すべて任意のスコープに持つが、求めていないものは付かない）。ゲートウェイが受け付ける。
        let exchanged = exchange_for_user(&http, &realm.token_endpoint(), &b2, &secret, &token)
            .await
            .expect("exchange");
        let got = claims(&exchanged.access_token);
        assert_eq!(got["sub"], ALICE, "{got}");
        assert_eq!(got["azp"], b2.as_str(), "{got}");
        assert_eq!(got["tenant"], "a-corp", "{got}");
        let identity = verify_gateway_token(
            &exchanged.access_token,
            &realm.key_resolver(),
            &gateway_cfg(&realm),
        )
        .await
        .expect("verify exchanged");
        assert_eq!(identity.user_sub, ALICE);
        assert_eq!(identity.client_id, b2);
        assert_eq!(identity.token_scopes, [CapabilityScope::DataRead]);

        // 別のアプリの B2 は、このアプリの利用者のトークンを交換できない。
        let refused = exchange_for_user(
            &http,
            &realm.token_endpoint(),
            &other_b2,
            &other_secret,
            &token,
        )
        .await;
        assert!(
            matches!(refused, Err(GatewayError::Forbidden(_))),
            "{refused:?}"
        );

        // アンインストール（B1 を無効）の後は、発行済みのトークンも交換できない。
        oauth.set_enabled(&b1, false).await.expect("disable b1");
        let after_uninstall =
            exchange_for_user(&http, &realm.token_endpoint(), &b2, &secret, &token).await;
        assert!(
            matches!(after_uninstall, Err(GatewayError::Forbidden(_))),
            "{after_uninstall:?}"
        );
    };
    run_then(body, async {
        for c in [&b1, &b2, &other_b2] {
            realm.delete_client(c).await;
        }
    })
    .await;
}

/// 交換の相手を aud に足していない前の版の B1 のトークンは交換できず、起動時の照合と同じ直し方で揃えると交換できる。
#[tokio::test]
async fn repairing_a_previous_b1_lets_its_b2_exchange() {
    let Some(realm) = TestRealm::from_env() else {
        eprintln!("KEYCLOAK_TEST_URL 未設定のためスキップ");
        return;
    };
    let app = Uuid::new_v4();
    let (b1, b2) = (format!("app-{app}-b1"), format!("app-{app}-b2"));
    let oauth = realm.oauth_client();
    let http = reqwest::Client::new();
    let redirects = [SHELL_CALLBACK.to_string()];
    let body = async {
        // 前の版の形（交換の相手を aud に足さない）。
        oauth
            .register(ClientKind::PublicPkce, &b1, "経費", &redirects, None)
            .await
            .expect("register b1");
        let secret = oauth
            .register(ClientKind::Confidential, &b2, "経費", &[], None)
            .await
            .expect("register b2")
            .client_secret
            .expect("b2 secret");
        let token = realm
            .user_token(&b1, SHELL_CALLBACK, "alice", "openid data.read")
            .await
            .expect("token");
        let refused = exchange_for_user(&http, &realm.token_endpoint(), &b2, &secret, &token).await;
        assert!(
            matches!(refused, Err(GatewayError::Forbidden(_))),
            "{refused:?}"
        );

        assert!(oauth
            .repair(ClientKind::PublicPkce, &b1, "経費", &redirects, Some(&b2))
            .await
            .expect("repair"));
        let token = realm
            .user_token(&b1, SHELL_CALLBACK, "alice", "openid data.read")
            .await
            .expect("token after repair");
        let exchanged = exchange_for_user(&http, &realm.token_endpoint(), &b2, &secret, &token)
            .await
            .expect("exchange after repair");
        assert_eq!(claims(&exchanged.access_token)["sub"], ALICE);
    };
    run_then(body, async {
        realm.delete_client(&b1).await;
        realm.delete_client(&b2).await;
    })
    .await;
}
