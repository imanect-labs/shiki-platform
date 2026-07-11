//! collab の結合テスト（update log/snapshot 永続化・収束・圧縮。実 Postgres が必要）。
//!
//! `STORAGE_TEST_DATABASE_URL` が設定されている時のみ実行し、未設定ならスキップする
//! （素の `cargo test` を壊さない）。収束系はプロセス内 yrs のみで完結する。

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

use collab::store::COMPACT_EVERY;
use collab::{DocStore, LiveDoc, PersistedDoc};
use sqlx::{postgres::PgPoolOptions, PgPool};
use uuid::Uuid;
use yrs::updates::decoder::Decode;
use yrs::{Doc, GetString, ReadTxn, StateVector, Text, Transact, Update};

async fn setup_pool() -> Option<PgPool> {
    let Ok(db_url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("Postgres へ接続できること");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("マイグレーション適用");
    Some(pool)
}

/// collab_doc は node への FK を持つため、テスト用の blob + file ノード行を直接播種する。
async fn seed_file_node(pool: &PgPool, org: &str, tenant: &str) -> Uuid {
    let sha = format!("{:0>64}", hex::encode(Uuid::new_v4().as_bytes()));
    sqlx::query(
        "INSERT INTO blob (tenant_id, org, sha256, size_bytes, content_type, object_key, refcount)
         VALUES ($4, $1, $2, 1, 'text/markdown', $3, 1)",
    )
    .bind(org)
    .bind(&sha)
    .bind(format!("{tenant}/{org}/{sha}"))
    .bind(tenant)
    .execute(pool)
    .await
    .expect("blob 播種");
    let node_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO node (id, org, tenant_id, kind, name, blob_sha256, size_bytes, content_type, created_by)
         VALUES ($1, $2, $3, 'file', $1::text || '-note.md', $4, 1, 'text/markdown', 'tester')",
    )
    .bind(node_id)
    .bind(org)
    .bind(tenant)
    .bind(&sha)
    .execute(pool)
    .await
    .expect("node 播種");
    node_id
}

/// 編集主体のテスト用 AuthContext（author 記録・保存主体）。
fn test_ctx() -> authz::AuthContext {
    authz::AuthContext::new(
        authz::Principal {
            kind: authz::PrincipalKind::User,
            id: "tester".into(),
            email: None,
            groups: vec!["/acme".into()],
            roles: vec![],
            tenant_id: None,
        },
        "acme".into(),
        "default".into(),
    )
}

/// 空の永続状態（LiveDoc をメモリ内だけで使うテスト用）。
fn empty_persisted() -> PersistedDoc {
    PersistedDoc {
        snapshot: None,
        snapshot_seq: 0,
        next_seq: 1,
        updates: vec![],
        saved_node_version: None,
    }
}

/// クライアント Doc の全変更を LiveDoc に適用し、LiveDoc の全状態を返す補助。
fn client_update(doc: &Doc, since: &StateVector) -> Vec<u8> {
    doc.transact().encode_state_as_update_v1(since)
}

/// 受け入れ条件: 2 クライアントの並行編集が収束する（オフライン→再接続を含む）。
///
/// クライアント A/B が独立に編集（オフライン相当）→それぞれの update をサーバ
/// [`LiveDoc`] に適用→サーバ状態を双方に配って全員が同一文書になることを検証する。
#[test]
fn concurrent_edits_converge_via_live_doc() {
    let live = LiveDoc::restore(Uuid::new_v4(), &empty_persisted()).expect("restore");

    // クライアント A: 先頭に "hello "、クライアント B: "world"（並行・互いを知らない）。
    let doc_a = Doc::new();
    let text_a = doc_a.get_or_insert_text("t");
    text_a.insert(&mut doc_a.transact_mut(), 0, "hello ");
    let doc_b = Doc::new();
    let text_b = doc_b.get_or_insert_text("t");
    text_b.insert(&mut doc_b.transact_mut(), 0, "world");

    // オフライン編集がそれぞれ到着（順不同でも CRDT は収束する）。
    live.apply_update_bytes(&client_update(&doc_b, &StateVector::default()))
        .expect("B の update 適用");
    live.apply_update_bytes(&client_update(&doc_a, &StateVector::default()))
        .expect("A の update 適用");

    // 再接続: 各クライアントはサーバ diff を取り込む（sync step1/2 相当）。
    let sv_a = doc_a.transact().state_vector();
    let diff_a = live.diff(&sv_a).expect("diff for A");
    doc_a
        .transact_mut()
        .apply_update(Update::decode_v1(&diff_a).expect("decode"))
        .expect("A へ適用");
    let sv_b = doc_b.transact().state_vector();
    let diff_b = live.diff(&sv_b).expect("diff for B");
    doc_b
        .transact_mut()
        .apply_update(Update::decode_v1(&diff_b).expect("decode"))
        .expect("B へ適用");

    let final_a = text_a.get_string(&doc_a.transact());
    let final_b = text_b.get_string(&doc_b.transact());
    assert_eq!(final_a, final_b, "A/B が同一文書に収束すること");
    assert!(final_a.contains("hello") && final_a.contains("world"));
}

/// 不正なバイト列（敵対的入力）は適用を拒否する（fail-closed）。
#[test]
fn garbage_update_is_rejected() {
    let live = LiveDoc::restore(Uuid::new_v4(), &empty_persisted()).expect("restore");
    let err = live.apply_update_bytes(&[0xFF, 0x00, 0xAB, 0xCD]);
    assert!(err.is_err(), "壊れた update は拒否されること");
}

/// 受け入れ条件: update log が snapshot に圧縮され無限肥大しない。
/// 併せて「snapshot＋残 update からの復元」が元の文書に一致することを検証する。
#[tokio::test]
async fn update_log_compacts_into_snapshot() {
    let Some(pool) = setup_pool().await else {
        return;
    };
    let node_id = seed_file_node(&pool, "acme", "default").await;
    let store = DocStore::new(pool.clone());
    store
        .load_or_init(node_id, "acme", "default")
        .await
        .expect("init");

    let live = LiveDoc::restore(node_id, &empty_persisted()).expect("restore");
    let client = Doc::new();
    let text = client.get_or_insert_text("t");

    // COMPACT_EVERY を跨ぐ回数の編集を 1 文字ずつ送る（キーストローク相当）。
    let total = COMPACT_EVERY + 5;
    let mut last_sv = StateVector::default();
    for i in 0..total {
        text.insert(
            &mut client.transact_mut(),
            i as u32,
            if i % 2 == 0 { "a" } else { "b" },
        );
        let update = client_update(&client, &last_sv);
        last_sv = client.transact().state_vector();
        live.apply_and_persist(&store, &update, &test_ctx())
            .await
            .expect("適用と追記");
    }

    // 圧縮済み: 残 update は COMPACT_EVERY 未満まで減っていること（無限肥大しない）。
    let pending = store.pending_update_count(node_id).await.expect("count");
    assert!(
        pending < COMPACT_EVERY,
        "update log が圧縮されていること（残 {pending} 件）"
    );

    // snapshot＋残 update からの復元が元文書に一致すること。
    let persisted = store.load(node_id, "default").await.expect("reload");
    assert!(persisted.snapshot.is_some(), "snapshot が作られていること");
    let restored = LiveDoc::restore(node_id, &persisted).expect("restore from snapshot");
    let full = restored.full_state().expect("full state");
    let check = Doc::new();
    let check_text = check.get_or_insert_text("t");
    check
        .transact_mut()
        .apply_update(Update::decode_v1(&full).expect("decode"))
        .expect("復元適用");
    assert_eq!(
        check_text.get_string(&check.transact()),
        text.get_string(&client.transact()),
        "復元した文書がクライアントと一致すること"
    );

    // 最終圧縮（アンロード時パス）: 残 update がゼロになること。
    live.compact_now(&store).await.expect("最終圧縮");
    let pending = store.pending_update_count(node_id).await.expect("count");
    assert_eq!(pending, 0, "最終圧縮で log が空になること");
    let persisted = store.load(node_id, "default").await.expect("reload");
    let restored = LiveDoc::restore(node_id, &persisted).expect("restore");
    assert_eq!(
        restored.full_state().expect("full"),
        full,
        "最終圧縮後も全状態が保たれること"
    );
}

/// tenant_id スコープ: 異なるテナントからは collab_doc を引けない。
#[tokio::test]
async fn load_is_tenant_scoped() {
    let Some(pool) = setup_pool().await else {
        return;
    };
    let node_id = seed_file_node(&pool, "acme", "default").await;
    let store = DocStore::new(pool.clone());
    store
        .load_or_init(node_id, "acme", "default")
        .await
        .expect("init");
    let err = store.load(node_id, "other-tenant").await;
    assert!(err.is_err(), "他テナントからのロードは NotFound になること");
}
