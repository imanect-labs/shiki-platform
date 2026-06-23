//! OpenFGA check の正例・負例の結合テスト。
//!
//! 実 OpenFGA が必要。`OPENFGA_TEST_URL`（例: `http://localhost:8080`）が
//! 設定されている時のみ実行し、未設定なら early-return でスキップする
//! （ローカルの素の `cargo test` を壊さないため）。CI の compose smoke で実走する。

use authz::{
    client::{OpenFgaClient, OpenFgaConfig},
    object::{FgaObject, Subject},
    vocab::Relation,
    AuthzClient,
};

fn model_json() -> serde_json::Value {
    let raw = include_str!("../model/authorization-model.json");
    serde_json::from_str(raw).expect("model JSON が不正")
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

    let alice = Subject::user("alice");
    let acme = FgaObject::organization("acme");
    let other = FgaObject::organization("other");

    // alice を acme の member として付与。
    client
        .write_tuple(&alice, Relation::Member, &acme)
        .await
        .expect("tuple 書き込み成功");

    // 正例: alice は acme の member。
    assert!(client.check(&alice, Relation::Member, &acme).await.unwrap());

    // 負例: alice は other org の member ではない。
    assert!(!client
        .check(&alice, Relation::Member, &other)
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

    let bob = Subject::user("bob");
    let acme = FgaObject::organization("acme");

    // 冪等な write: 同じ tuple を 2 回書いても成功扱い（dual-write の再試行で収束）。
    client
        .write_tuple(&bob, Relation::Member, &acme)
        .await
        .expect("1 回目の write 成功");
    client
        .write_tuple(&bob, Relation::Member, &acme)
        .await
        .expect("既存 tuple の再 write も成功扱い（冪等）");
    assert!(client.check(&bob, Relation::Member, &acme).await.unwrap());

    // 冪等な delete: 2 回削除しても成功扱い（不在 delete を許容）。
    client
        .delete_tuple(&bob, Relation::Member, &acme)
        .await
        .expect("1 回目の delete 成功");
    client
        .delete_tuple(&bob, Relation::Member, &acme)
        .await
        .expect("不在 tuple の再 delete も成功扱い（冪等）");
    assert!(!client.check(&bob, Relation::Member, &acme).await.unwrap());
}
