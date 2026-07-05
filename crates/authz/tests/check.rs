//! OpenFGA check の正例・負例の結合テスト。
//!
//! 実 OpenFGA が必要。`OPENFGA_TEST_URL`（例: `http://localhost:8080`）が
//! 設定されている時のみ実行し、未設定なら early-return でスキップする
//! （ローカルの素の `cargo test` を壊さないため）。CI の compose smoke で実走する。

// テストコード: pedantic/安全系 lint は本番コードのみ厳格化する方針のため許容する。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::pedantic,
    clippy::cognitive_complexity
)]

use authz::{
    client::{OpenFgaClient, OpenFgaConfig},
    vocab::Relation,
    AuthContext, AuthzClient, Consistency, Principal,
};

fn model_json() -> serde_json::Value {
    let raw = include_str!("../model/authorization-model.json");
    serde_json::from_str(raw).expect("model JSON が不正")
}

/// テナント名前空間化（SAAS.1）された識別子を組むための最小 `AuthContext`。
/// アプリ同様 `ctx.ns()` 経由でしか FGA 識別子を構築できないため、テストも同経路を使う。
fn ctx_for(tenant_id: &str, user_id: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            id: user_id.to_string(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant_id.to_string()),
        },
        "acme".to_string(),
        tenant_id.to_string(),
    )
}

#[tokio::test]
async fn check_allows_member_and_denies_other_org() {
    let Ok(base_url) = std::env::var("OPENFGA_TEST_URL") else {
        eprintln!("OPENFGA_TEST_URL 未設定のためスキップ");
        return;
    };

    // store 名はテスト毎にユニークにして他テストと干渉させない。
    let store_name = format!("shiki-test-{}", uuid::Uuid::new_v4());
    let http = reqwest::Client::new();
    let config = OpenFgaConfig {
        base_url,
        store_name,
    };

    let client = OpenFgaClient::connect(http, &config, &model_json())
        .await
        .expect("OpenFGA へ接続できること");

    // tenant `t1` の名前空間で識別子を組む（`user:t1|alice` / `organization:t1|acme`）。
    let ctx = ctx_for("t1", "alice");
    let alice = ctx.subject();
    let acme = ctx.ns().organization("acme");
    let other = ctx.ns().organization("other");

    // alice を acme の member として付与（新規付与なので changed=true）。
    assert!(client
        .write_tuple(&alice, Relation::Member, &acme)
        .await
        .expect("tuple 書き込み成功"));

    // 正例: alice は acme の member（強整合で書込直後を確実に観測）。
    assert!(client
        .check(
            &alice,
            Relation::Member,
            &acme,
            Consistency::HigherConsistency
        )
        .await
        .unwrap());

    // 負例: alice は other org の member ではない。
    assert!(!client
        .check(
            &alice,
            Relation::Member,
            &other,
            Consistency::HigherConsistency
        )
        .await
        .unwrap());
}

#[tokio::test]
async fn write_and_delete_tuple_are_idempotent() {
    let Ok(base_url) = std::env::var("OPENFGA_TEST_URL") else {
        eprintln!("OPENFGA_TEST_URL 未設定のためスキップ");
        return;
    };
    let store_name = format!("shiki-test-{}", uuid::Uuid::new_v4());
    let http = reqwest::Client::new();
    let config = OpenFgaConfig {
        base_url,
        store_name,
    };
    let client = OpenFgaClient::connect(http, &config, &model_json())
        .await
        .expect("OpenFGA へ接続できること");

    let ctx = ctx_for("t1", "bob");
    let bob = ctx.subject();
    let acme = ctx.ns().organization("acme");

    // 冪等な write: 1 回目は実書込（true）、2 回目は既存 no-op（false）。
    assert!(client
        .write_tuple(&bob, Relation::Member, &acme)
        .await
        .expect("1 回目の write 成功"));
    assert!(!client
        .write_tuple(&bob, Relation::Member, &acme)
        .await
        .expect("既存 tuple の再 write も成功扱い（冪等）"));
    assert!(client
        .check(
            &bob,
            Relation::Member,
            &acme,
            Consistency::HigherConsistency
        )
        .await
        .unwrap());

    // 冪等な delete: 1 回目は実削除（true）、2 回目は不在 no-op（false）。
    assert!(client
        .delete_tuple(&bob, Relation::Member, &acme)
        .await
        .expect("1 回目の delete 成功"));
    assert!(!client
        .delete_tuple(&bob, Relation::Member, &acme)
        .await
        .expect("不在 tuple の再 delete も成功扱い（冪等）"));
    assert!(!client
        .check(
            &bob,
            Relation::Member,
            &acme,
            Consistency::HigherConsistency
        )
        .await
        .unwrap());
}
