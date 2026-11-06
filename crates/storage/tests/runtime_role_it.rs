//! 実行時ロール（`shiki_runtime`・migration 0113）の権限の結合テスト（#1030）。
//!
//! shiki-server は実行時に `shiki_runtime` のメンバ（compose では `shiki_app`）で接続し、migration だけを
//! 所有者の接続で流す。ここでは所有者の接続で migration を流したあと、同じセッションで
//! `SET ROLE shiki_runtime` に切り替えて、実行時ロールに許すこと・許さないことを確かめる:
//!
//! - DML（SELECT / INSERT / UPDATE / DELETE）と sequence の利用はできる
//! - DDL・TRUNCATE・素の ANALYZE はできない（テーブル所有者でも superuser でもない）
//! - RLS を迂回しない（BYPASSRLS を持たない）
//! - パーティションの統計は `shiki_analyze` 経由でだけ取り直せ、data_* 以外は拒否される
//!
//! `STORAGE_TEST_DATABASE_URL`（所有者の接続）未設定ならスキップ。

// テストコード: pedantic/安全系 lint は本番コードのみ厳格化する方針のため許容する。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::pedantic
)]

use sqlx::postgres::PgPoolOptions;
use sqlx::{Connection, PgConnection};

const PERMISSION_DENIED: &str = "42501";

/// 所有者の接続で migration を流し、`shiki_runtime` に切り替えた接続を返す。
async fn runtime_conn() -> Option<PgConnection> {
    let Ok(db_url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&db_url)
        .await
        .expect("connect");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    pool.close().await;
    let mut conn = PgConnection::connect(&db_url).await.expect("connect");
    sqlx::query("SET ROLE shiki_runtime")
        .execute(&mut conn)
        .await
        .expect("SET ROLE shiki_runtime（migration 0113 がロールを作る）");
    Some(conn)
}

/// SQL がエラーコードで失敗することを確かめる。
async fn assert_sqlstate(conn: &mut PgConnection, sql: &str, code: &str) {
    let err = sqlx::query(sql)
        .execute(&mut *conn)
        .await
        .expect_err(&format!("拒否されるはず: {sql}"));
    let db = err.as_database_error().expect("DB エラー");
    assert_eq!(db.code().as_deref(), Some(code), "{sql}: {db}");
}

/// data_record のパーティションを 1 つ（regclass の文字列）。
async fn some_data_partition(conn: &mut PgConnection) -> String {
    sqlx::query_scalar(
        "SELECT c.oid::regclass::text FROM pg_inherits i \
           JOIN pg_class c ON c.oid = i.inhrelid \
           JOIN pg_class p ON p.oid = i.inhparent \
          WHERE p.relname = 'data_record' ORDER BY 1 LIMIT 1",
    )
    .fetch_one(conn)
    .await
    .expect("data_record のパーティション")
}

#[tokio::test]
async fn runtime_role_is_not_privileged() {
    let Some(mut conn) = runtime_conn().await else {
        return;
    };
    let (is_super, bypass_rls): (bool, bool) =
        sqlx::query_as("SELECT rolsuper, rolbypassrls FROM pg_roles WHERE rolname = current_user")
            .fetch_one(&mut conn)
            .await
            .unwrap();
    assert!(!is_super, "実行時ロールは superuser でない");
    assert!(!bypass_rls, "実行時ロールは RLS を迂回しない");
}

#[tokio::test]
async fn runtime_role_can_run_dml_and_sequences() {
    let Some(mut conn) = runtime_conn().await else {
        return;
    };
    let mut tx = conn.begin().await.unwrap();
    let tenant = format!("t{}", uuid::Uuid::new_v4().simple());
    sqlx::query(
        "INSERT INTO tenant (tenant_id, org, display_name, status) VALUES ($1, 'org', 'role it', 'active')",
    )
    .bind(&tenant)
    .execute(&mut *tx)
    .await
    .expect("INSERT");
    sqlx::query("UPDATE tenant SET display_name = 'renamed' WHERE tenant_id = $1")
        .bind(&tenant)
        .execute(&mut *tx)
        .await
        .expect("UPDATE");
    let name: String = sqlx::query_scalar("SELECT display_name FROM tenant WHERE tenant_id = $1")
        .bind(&tenant)
        .fetch_one(&mut *tx)
        .await
        .expect("SELECT");
    assert_eq!(name, "renamed");
    sqlx::query("DELETE FROM tenant WHERE tenant_id = $1")
        .bind(&tenant)
        .execute(&mut *tx)
        .await
        .expect("DELETE");
    // sequence（採番）も使える。
    let seq: String = sqlx::query_scalar(
        "SELECT c.oid::regclass::text FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
          WHERE c.relkind = 'S' AND n.nspname = 'public' ORDER BY 1 LIMIT 1",
    )
    .fetch_one(&mut *tx)
    .await
    .expect("sequence がある");
    sqlx::query("SELECT nextval($1::regclass)")
        .bind(&seq)
        .execute(&mut *tx)
        .await
        .expect("nextval");
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn runtime_role_cannot_run_ddl_truncate_or_analyze() {
    let Some(mut conn) = runtime_conn().await else {
        return;
    };
    assert_sqlstate(
        &mut conn,
        "CREATE TABLE runtime_role_probe (id int)",
        PERMISSION_DENIED,
    )
    .await;
    assert_sqlstate(
        &mut conn,
        "ALTER TABLE tenant ADD COLUMN probe int",
        "42501",
    )
    .await;
    assert_sqlstate(&mut conn, "DROP TABLE tenant", "42501").await;
    assert_sqlstate(&mut conn, "TRUNCATE audit_log", PERMISSION_DENIED).await;

    // 素の ANALYZE は所有者でないので黙って飛ばされる（エラーにならず WARNING だけ）。
    // 統計を取り直す経路は shiki_analyze だけにする（下のテスト）。飛ばされたことは、ANALYZE が
    // 同期的に書き換える pg_class.reltuples（未収集なら -1）で確かめる。
    let probe = format!("runtime_role_analyze_{}", uuid::Uuid::new_v4().simple());
    sqlx::query("RESET ROLE").execute(&mut conn).await.unwrap();
    sqlx::query(&format!(
        "CREATE TABLE {probe} AS SELECT g AS id FROM generate_series(1, 100) g"
    ))
    .execute(&mut conn)
    .await
    .unwrap();
    sqlx::query(&format!("GRANT SELECT ON {probe} TO shiki_runtime"))
        .execute(&mut conn)
        .await
        .unwrap();
    sqlx::query("SET ROLE shiki_runtime")
        .execute(&mut conn)
        .await
        .unwrap();
    sqlx::query(&format!("ANALYZE {probe}"))
        .execute(&mut conn)
        .await
        .expect("素の ANALYZE はエラーにならず飛ばされる");
    sqlx::query("RESET ROLE").execute(&mut conn).await.unwrap();
    let reltuples: f32 = sqlx::query_scalar("SELECT reltuples FROM pg_class WHERE relname = $1")
        .bind(&probe)
        .fetch_one(&mut conn)
        .await
        .unwrap();
    sqlx::query(&format!("DROP TABLE {probe}"))
        .execute(&mut conn)
        .await
        .unwrap();
    assert!(
        reltuples < 0.0,
        "実行時ロールの素の ANALYZE で統計が取り直された（reltuples = {reltuples}）"
    );
}

#[tokio::test]
async fn shiki_analyze_is_limited_to_data_tables() {
    let Some(mut conn) = runtime_conn().await else {
        return;
    };
    let partition = some_data_partition(&mut conn).await;
    for skip_locked in [false, true] {
        sqlx::query("SELECT shiki_analyze($1::regclass, $2)")
            .bind(&partition)
            .bind(skip_locked)
            .execute(&mut conn)
            .await
            .unwrap_or_else(|e| panic!("data_* の ANALYZE は通る（{partition}）: {e}"));
    }
    assert_sqlstate(
        &mut conn,
        "SELECT shiki_analyze('tenant'::regclass)",
        PERMISSION_DENIED,
    )
    .await;
    assert_sqlstate(
        &mut conn,
        "SELECT shiki_analyze('pg_catalog.pg_class'::regclass)",
        PERMISSION_DENIED,
    )
    .await;
}

#[tokio::test]
async fn shiki_analyze_ignores_temp_objects_shadowing_the_catalog() {
    // SECURITY DEFINER 関数の search_path に pg_temp が無いと、呼び出し側が一時ビューで pg_class を
    // 覆い隠せる（定義者の権限で任意の関数を走らせる足場になる）。偽の pg_class / pg_namespace を
    // 置いても、data_* 以外の表は拒否されることを確かめる。
    let Some(mut conn) = runtime_conn().await else {
        return;
    };
    sqlx::query(
        "CREATE TEMP VIEW pg_class AS \
         SELECT c.oid, 'data_fake'::name AS relname, c.relnamespace FROM pg_catalog.pg_class c",
    )
    .execute(&mut conn)
    .await
    .expect("一時ビュー");
    sqlx::query(
        "CREATE TEMP VIEW pg_namespace AS \
         SELECT n.oid, 'public'::name AS nspname FROM pg_catalog.pg_namespace n",
    )
    .execute(&mut conn)
    .await
    .expect("一時ビュー");
    assert_sqlstate(
        &mut conn,
        "SELECT shiki_analyze('tenant'::regclass)",
        PERMISSION_DENIED,
    )
    .await;
}

#[tokio::test]
async fn runtime_role_cannot_touch_the_migration_ledger() {
    let Some(mut conn) = runtime_conn().await else {
        return;
    };
    assert_sqlstate(
        &mut conn,
        "SELECT 1 FROM _sqlx_migrations LIMIT 1",
        PERMISSION_DENIED,
    )
    .await;
    assert_sqlstate(&mut conn, "DELETE FROM _sqlx_migrations", PERMISSION_DENIED).await;
}
