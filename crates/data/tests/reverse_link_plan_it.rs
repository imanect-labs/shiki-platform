//! 参照先の変更から参照元を引く読み方の計画の形（独立レビュー R2-A・R2-C・実 PG）。
//!
//! - 片方向リンク（対称列なし）の参照元の逆引き（材料化した派生列の再計算待ち・派生値の再配信）は、
//!   `data_link_dst_idx`（migrations/0101）で引く。主キーでは `dst_record` で絞れず、そのリンク列の全行を
//!   書込 Tx の中で読んでいた。
//! - 再計算待ちの 1 回分は主キーの順に `LIMIT` 件だけ読む（全待ち行を読んで並べ替えない）。
//!
//! 統計（ANALYZE）を書き換えるので、他のテストと計画が干渉しないよう独立したテストバイナリにする。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use serde_json::Value;
use sqlx::PgPool;

/// 計画に現れる索引名と、ノードの種別。
fn walk(plan: &Value, idx: &mut Vec<String>, nodes: &mut Vec<String>) {
    match plan {
        Value::Object(m) => {
            if let Some(Value::String(n)) = m.get("Index Name") {
                idx.push(n.clone());
            }
            if let Some(Value::String(n)) = m.get("Node Type") {
                nodes.push(n.clone());
            }
            for v in m.values() {
                walk(v, idx, nodes);
            }
        }
        Value::Array(a) => a.iter().for_each(|v| walk(v, idx, nodes)),
        _ => {}
    }
}

/// `binds` を付けて custom / generic の両方の計画を取る。
async fn plans(
    pool: &PgPool,
    sql: &str,
    bind: impl Fn(
        sqlx::query::QueryScalar<'_, sqlx::Postgres, Value, sqlx::postgres::PgArguments>,
    )
        -> sqlx::query::QueryScalar<'_, sqlx::Postgres, Value, sqlx::postgres::PgArguments>,
) -> Vec<(bool, Value)> {
    let text = format!("EXPLAIN (FORMAT JSON) {sql}");
    let mut out = Vec::new();
    for generic in [false, true] {
        let mut tx = pool.begin().await.unwrap();
        let mode = if generic {
            "SET LOCAL plan_cache_mode = force_generic_plan"
        } else {
            "SET LOCAL plan_cache_mode = force_custom_plan"
        };
        sqlx::query(mode).execute(&mut *tx).await.unwrap();
        let plan: Value = bind(sqlx::query_scalar(&text))
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        tx.rollback().await.unwrap();
        out.push((generic, plan));
    }
    out
}

#[tokio::test]
async fn reverse_link_lookup_uses_the_destination_index() {
    let Some(pool) = common::setup().await else {
        return;
    };
    // 他のテストと重ならない内部キー（索引表には外部キーが無い）。
    let (tenant_int, table_int, dst_table) =
        (2_000_000_002_i32, 9_000_000_002_i64, 9_000_000_003_i64);
    let clean = async |pool: &PgPool| {
        for t in ["data_link", "data_link_field"] {
            sqlx::query(&format!("DELETE FROM {t} WHERE tenant_int = $1"))
                .bind(tenant_int)
                .execute(pool)
                .await
                .unwrap();
        }
    };
    clean(&pool).await;
    sqlx::query(
        "INSERT INTO data_link_field (tenant_int, table_int, field_id, single_valued) \
         VALUES ($1, $2, 16, false)",
    )
    .bind(tenant_int)
    .bind(table_int)
    .execute(&pool)
    .await
    .unwrap();
    // 1 本のリンク列に 6 万辺（参照先は 6,000 件に散る）。
    sqlx::query(
        "INSERT INTO data_link (tenant_int, table_int, field_id, src_record, ord, dst_table_int, \
                                dst_record, is_reverse, single_valued) \
         SELECT $1, $2, 16, r, 0, $3, r % 6000, false, false FROM generate_series(1, 60000) r",
    )
    .bind(tenant_int)
    .bind(table_int)
    .bind(dst_table)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("ANALYZE data_link")
        .execute(&pool)
        .await
        .unwrap();
    let children: Vec<String> = sqlx::query_scalar(
        "SELECT c.relname::text FROM pg_inherits i JOIN pg_class c ON c.oid = i.inhrelid \
         WHERE i.inhparent = 'data_link_dst_idx'::regclass",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(
        !children.is_empty(),
        "data_link_dst_idx がパーティションへ伝播している"
    );
    let targets: Vec<i64> = vec![11, 12, 13];
    for (generic, plan) in plans(&pool, data::REFERRERS_BY_TARGET_SQL, |q| {
        q.bind(tenant_int)
            .bind(table_int)
            .bind(16_i16)
            .bind(dst_table)
            .bind(targets.clone())
    })
    .await
    {
        let (mut idx, mut nodes) = (Vec::new(), Vec::new());
        walk(&plan, &mut idx, &mut nodes);
        assert!(
            !idx.is_empty() && idx.iter().all(|n| children.contains(n)),
            "参照先の索引で引く（リンク列の全行を読まない・generic={generic}）: {idx:?} {plan}"
        );
    }
    // 実際の答えも正しい（11・12・13 を指す参照元は各 10 件）。
    let got: Vec<i64> = sqlx::query_scalar(data::REFERRERS_BY_TARGET_SQL)
        .bind(tenant_int)
        .bind(table_int)
        .bind(16_i16)
        .bind(dst_table)
        .bind(&targets)
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(got.len(), 30);
    assert!(got.iter().all(|r| targets.contains(&(r % 6000))));
    clean(&pool).await;
}

#[tokio::test]
async fn derived_pending_step_reads_in_primary_key_order_without_sorting() {
    let Some(pool) = common::setup().await else {
        return;
    };
    let (tenant_int, table_int) = (2_000_000_003_i32, 9_000_000_004_i64);
    let clean = async |pool: &PgPool| {
        sqlx::query("DELETE FROM data_derived_pending WHERE tenant_int = $1")
            .bind(tenant_int)
            .execute(pool)
            .await
            .unwrap();
    };
    clean(&pool).await;
    // 3 列 × 3 万行の待ち。
    sqlx::query(
        "INSERT INTO data_derived_pending (tenant_int, table_int, field_id, record_int) \
         SELECT $1, $2, f::int2, r FROM generate_series(1, 30000) r, generate_series(16, 18) f",
    )
    .bind(tenant_int)
    .bind(table_int)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("ANALYZE data_derived_pending")
        .execute(&pool)
        .await
        .unwrap();
    for (generic, plan) in plans(&pool, data::DERIVED_PENDING_STEP_SQL, |q| {
        q.bind(tenant_int).bind(table_int).bind(500_i64)
    })
    .await
    {
        let (mut idx, mut nodes) = (Vec::new(), Vec::new());
        walk(&plan, &mut idx, &mut nodes);
        assert_eq!(
            idx,
            vec!["data_derived_pending_pkey".to_string()],
            "主キーを前から読む（generic={generic}）: {plan}"
        );
        assert!(
            !nodes.iter().any(|n| n.contains("Sort")),
            "全待ち行を並べ替えない（generic={generic}）: {nodes:?}"
        );
    }
    clean(&pool).await;
}
