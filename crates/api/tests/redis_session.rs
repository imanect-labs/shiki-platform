//! `RedisSessionStore` の結合テスト（実 Redis が必要）。
//!
//! `REDIS_TEST_URL`（例: `redis://localhost:6379`）が設定されている時のみ実行し、
//! 未設定なら early-return でスキップする（ローカルの素の `cargo test` を壊さないため）。
//! CI の coverage ジョブで実 Redis に対して実走し、redis_store.rs の実経路をカバーする。

use std::time::Duration;

use api::session::{RedisSessionStore, SessionRecord, SessionStore};
use authz::Principal;

fn record(csrf: &str) -> SessionRecord {
    SessionRecord {
        principal: Principal {
            id: "00000000-0000-0000-0000-000000000001".into(),
            email: Some("alice@acme.example".into()),
            groups: vec!["/acme".into()],
            roles: vec!["eng".into()],
            tenant_id: Some("default".into()),
        },
        tenant_id: "default".into(),
        access_token: "access".into(),
        refresh_token: Some("refresh".into()),
        id_token: None,
        access_expires_at: 1_900_000_000,
        csrf_token: csrf.into(),
    }
}

#[tokio::test]
async fn redis_put_get_update_delete_and_tenant_scope() {
    let Ok(redis_url) = std::env::var("REDIS_TEST_URL") else {
        eprintln!("REDIS_TEST_URL 未設定のためスキップ");
        return;
    };
    let store = RedisSessionStore::connect(&redis_url)
        .await
        .expect("Redis へ接続できること");

    // テスト毎にユニークな session id にして他テストと干渉させない。
    let sid = format!("it-{}", uuid::Uuid::new_v4());
    let ttl = Duration::from_secs(60);

    // put → get で round-trip。
    store
        .put("default", &sid, &record("csrf-1"), ttl)
        .await
        .expect("put 成功");
    let got = store.get("default", &sid).await.unwrap().expect("存在する");
    assert_eq!(got.csrf_token, "csrf-1");
    assert_eq!(got.access_token, "access");

    // 別テナントスコープからは引けない（共用プールの論理分離）。
    assert!(store.get("other-tenant", &sid).await.unwrap().is_none());

    // update_if_present: 既存なら更新して true。
    let updated = store
        .update_if_present("default", &sid, &record("csrf-2"), ttl)
        .await
        .unwrap();
    assert!(updated);
    assert_eq!(
        store
            .get("default", &sid)
            .await
            .unwrap()
            .unwrap()
            .csrf_token,
        "csrf-2"
    );

    // delete 後は取得不能、かつ update_if_present は false（復活させない）。
    store.delete("default", &sid).await.expect("delete 成功");
    assert!(store.get("default", &sid).await.unwrap().is_none());
    let resurrect = store
        .update_if_present("default", &sid, &record("csrf-3"), ttl)
        .await
        .unwrap();
    assert!(!resurrect, "削除済みセッションを復活させない");
    assert!(store.get("default", &sid).await.unwrap().is_none());
}
