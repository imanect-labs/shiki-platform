//! `migrate::retenant_object_tuples` の結合テスト（実 OpenFGA・#89）。
//!
//! `OPENFGA_TEST_URL` 設定時のみ実行（未設定は skip）。旧無印識別子（LEGACY）と
//! テナントリネームの両モードで、subject/object 双方が新名前空間へ移り旧タプルが
//! 消えること・他名前空間に触れないことを検証する。

use authz::{
    client::{OpenFgaClient, OpenFgaConfig},
    fga_http::FgaHttp,
    migrate::{retenant_object_tuples, FromNs},
    vocab::ObjectType,
    AuthContext, AuthzClient, Consistency, Principal,
};

fn model_json() -> serde_json::Value {
    serde_json::from_str(include_str!("../model/authorization-model.json")).unwrap()
}

fn ctx_for(tenant: &str, user: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            id: user.into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        "org".into(),
        tenant.into(),
    )
}

async fn connect() -> Option<(OpenFgaClient, FgaHttp)> {
    let Ok(base_url) = std::env::var("OPENFGA_TEST_URL") else {
        eprintln!("OPENFGA_TEST_URL 未設定のためスキップ");
        return None;
    };
    let http = reqwest::Client::new();
    let client = OpenFgaClient::connect(
        http.clone(),
        &OpenFgaConfig {
            base_url: base_url.clone(),
            store_name: format!("shiki-retenant-{}", uuid::Uuid::new_v4()),
        },
        &model_json(),
    )
    .await
    .expect("OpenFGA 接続");
    let fga = FgaHttp::new(http, &base_url);
    Some((client, fga))
}

#[tokio::test]
async fn retenant_legacy_moves_tuples_into_namespace() {
    let Some((client, fga)) = connect().await else {
        return;
    };
    let (sid, mid) = (client.store_id().to_string(), client.model_id().to_string());

    // 旧無印タプル（SAAS.1 以前の形式）を直接投入する。
    fga.write_tuple(&sid, &mid, "user:alice", "owner", "folder:f1")
        .await
        .unwrap();
    fga.write_tuple(&sid, &mid, "role:sales#member", "viewer", "folder:f1")
        .await
        .unwrap();
    // 既に名前空間化済みの他テナントのタプル（触れてはいけない）。
    fga.write_tuple(&sid, &mid, "user:other|bob", "editor", "folder:f1")
        .await
        .unwrap();

    // dry-run: 何も変えず件数だけ返す。
    let (moved, skipped) = retenant_object_tuples(
        &client,
        ObjectType::Folder,
        "f1",
        &FromNs::Legacy,
        "t1",
        false,
    )
    .await
    .unwrap();
    assert_eq!(
        (moved, skipped),
        (2, 1),
        "dry-run の件数（移行 2・他名前空間 1）"
    );

    // execute: 移行実行。
    let (moved, skipped) = retenant_object_tuples(
        &client,
        ObjectType::Folder,
        "f1",
        &FromNs::Legacy,
        "t1",
        true,
    )
    .await
    .unwrap();
    assert_eq!((moved, skipped), (2, 1));

    // 新名前空間で check が通り、旧タプル由来では通らない。
    let ctx = ctx_for("t1", "alice");
    assert!(client
        .check(
            &ctx.subject(),
            authz::Relation::Owner,
            &ctx.ns().folder("f1"),
            Consistency::HigherConsistency,
        )
        .await
        .unwrap());
    // 旧 object にはもう移行対象タプルが無い（残るのは他名前空間の 1 本のみ）。
    let leftovers = fga.read_tuples(&sid, "folder:f1", None).await.unwrap();
    assert_eq!(
        leftovers.len(),
        1,
        "旧 object に残るのは他名前空間のタプルのみ"
    );
    assert_eq!(leftovers[0].user, "user:other|bob");

    // 冪等: 再実行は移行対象 0。
    let (moved, _) = retenant_object_tuples(
        &client,
        ObjectType::Folder,
        "f1",
        &FromNs::Legacy,
        "t1",
        true,
    )
    .await
    .unwrap();
    assert_eq!(moved, 0);
}

#[tokio::test]
async fn retenant_rename_moves_between_tenants() {
    let Some((client, fga)) = connect().await else {
        return;
    };
    let (sid, mid) = (client.store_id().to_string(), client.model_id().to_string());

    // cell 相当（default 名前空間）のタプルを実行時経路と同じ形で用意する。
    let cell = ctx_for("default", "alice");
    client
        .write_tuple(
            &cell.subject(),
            authz::Relation::Owner,
            &cell.ns().file("doc1"),
        )
        .await
        .unwrap();

    let (moved, skipped) = retenant_object_tuples(
        &client,
        ObjectType::File,
        "doc1",
        &FromNs::Tenant("default".into()),
        "acme",
        true,
    )
    .await
    .unwrap();
    assert_eq!((moved, skipped), (1, 0));

    // 新テナントで check が通り、旧テナントでは deny。
    let pool = ctx_for("acme", "alice");
    assert!(client
        .check(
            &pool.subject(),
            authz::Relation::Owner,
            &pool.ns().file("doc1"),
            Consistency::HigherConsistency,
        )
        .await
        .unwrap());
    assert!(!client
        .check(
            &cell.subject(),
            authz::Relation::Owner,
            &cell.ns().file("doc1"),
            Consistency::HigherConsistency,
        )
        .await
        .unwrap());
    // 旧 object のタプルは空。
    let leftovers = fga
        .read_tuples(&sid, "file:default|doc1", None)
        .await
        .unwrap();
    assert!(leftovers.is_empty());
    let _ = mid; // model_id は write 経由で使用済み。
}
