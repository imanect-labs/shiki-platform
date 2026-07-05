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
        keycloak_sid: Some("sso-session-1".into()),
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

#[tokio::test]
async fn redis_backchannel_logout_indices_and_jti() {
    let Ok(redis_url) = std::env::var("REDIS_TEST_URL") else {
        eprintln!("REDIS_TEST_URL 未設定のためスキップ");
        return;
    };
    let store = RedisSessionStore::connect(&redis_url)
        .await
        .expect("Redis へ接続できること");
    let ttl = Duration::from_secs(60);
    let uniq = uuid::Uuid::new_v4();
    let sub = format!("user-{uniq}");
    let sid_val = format!("sso-{uniq}");

    // 同一 sub / 同一 sid の 2 セッションを作る。
    let mut rec = record("csrf");
    rec.principal.id = sub.clone();
    rec.keycloak_sid = Some(sid_val.clone());
    let s1 = format!("bcl1-{uniq}");
    let s2 = format!("bcl2-{uniq}");
    store.put("default", &s1, &rec, ttl).await.unwrap();
    store.put("default", &s2, &rec, ttl).await.unwrap();

    // delete_by_sid: sid 一致の全セッションを失効。
    let n = store.delete_by_sid(&sid_val).await.unwrap();
    assert_eq!(n, 2, "sid 一致の 2 セッションが失効する");
    assert!(store.get("default", &s1).await.unwrap().is_none());
    assert!(store.get("default", &s2).await.unwrap().is_none());

    // delete_by_subject: 再作成後、sub で全セッション失効。
    let s3 = format!("bcl3-{uniq}");
    store.put("default", &s3, &rec, ttl).await.unwrap();
    let n = store.delete_by_subject(&sub).await.unwrap();
    assert!(n >= 1, "sub 一致セッションが失効する");
    assert!(store.get("default", &s3).await.unwrap().is_none());

    // register_jti: 初出は true、再送は false（リプレイ）。
    let jti = format!("jti-{uniq}");
    assert!(store.register_jti(&jti, ttl).await.unwrap(), "初出は受理");
    assert!(
        !store.register_jti(&jti, ttl).await.unwrap(),
        "再送はリプレイとして拒否"
    );
}
