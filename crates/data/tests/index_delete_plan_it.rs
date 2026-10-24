//! 索引行の削除文の計画の形（検証 D-6・実 PG）。
//!
//! バックフィルは 1 チャンクごとに (列, レコード) の組の索引行を消してから作り直す。新しい列は統計に
//! 載っていないため、`field_id = ANY … AND record_int = ANY …` の形では Postgres が値索引を選び、配列の
//! 要素ごとにその列の索引行を全部読み直した（30 万行で 1 回 3.5 秒・バックフィル 15 分）。削除文が
//! 統計の古さに関わらず主キーで組ごとに引くことを `EXPLAIN` で固定する。
//!
//! 統計（ANALYZE）を書き換えるので、他のテストと計画が干渉しないよう独立したテストバイナリにする。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use serde_json::Value;

fn index_names(plan: &Value, out: &mut Vec<String>) {
    match plan {
        Value::Object(m) => {
            if let Some(Value::String(n)) = m.get("Index Name") {
                out.push(n.clone());
            }
            for v in m.values() {
                index_names(v, out);
            }
        }
        Value::Array(a) => a.iter().for_each(|v| index_names(v, out)),
        _ => {}
    }
}

#[tokio::test]
async fn index_delete_uses_the_primary_key_even_with_stale_statistics() {
    let Some(pool) = common::setup().await else {
        return;
    };
    // 他のテストと重ならない内部キー（索引表には外部キーが無い）。
    let (tenant_int, table_int) = (2_000_000_001_i32, 9_000_000_001_i64);
    sqlx::query("DELETE FROM data_index_text WHERE tenant_int = $1")
        .bind(tenant_int)
        .execute(&pool)
        .await
        .unwrap();
    // 既存の列（16〜18）が少ないうちに統計を取り、その後で大きくする＋新しい列 19 を作る
    // （バックフィル中の列は統計に無い）。
    let insert = "INSERT INTO data_index_text (tenant_int, table_int, field_id, record_int, ord, val) \
                  SELECT $1, $2, f::int2, r, 0, 'v' || (r % 1000) \
                  FROM generate_series($3::int8, $4::int8) r, generate_series($5::int4, $6::int4) f";
    for (lo, hi, f0, f1) in [(1_i64, 300_i64, 16_i32, 18_i32)] {
        sqlx::query(insert)
            .bind(tenant_int)
            .bind(table_int)
            .bind(lo)
            .bind(hi)
            .bind(f0)
            .bind(f1)
            .execute(&pool)
            .await
            .unwrap();
    }
    sqlx::query("ANALYZE data_index_text")
        .execute(&pool)
        .await
        .unwrap();
    for (lo, hi, f0, f1) in [(301_i64, 100_000_i64, 16_i32, 18_i32), (1, 60_000, 19, 19)] {
        sqlx::query(insert)
            .bind(tenant_int)
            .bind(table_int)
            .bind(lo)
            .bind(hi)
            .bind(f0)
            .bind(f1)
            .execute(&pool)
            .await
            .unwrap();
    }
    let records: Vec<i64> = (60_001..=60_500).collect();
    let fields: Vec<i16> = vec![19];
    let sql = format!("EXPLAIN (FORMAT JSON) {}", data::text_index_delete_sql());
    for generic in [false, true] {
        let mut tx = pool.begin().await.unwrap();
        let mode = if generic {
            "SET LOCAL plan_cache_mode = force_generic_plan"
        } else {
            "SET LOCAL plan_cache_mode = force_custom_plan"
        };
        sqlx::query(mode).execute(&mut *tx).await.unwrap();
        let plan: Value = sqlx::query_scalar(&sql)
            .bind(tenant_int)
            .bind(table_int)
            .bind(&fields)
            .bind(&records)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        tx.rollback().await.unwrap();
        let mut idx = Vec::new();
        index_names(&plan, &mut idx);
        assert!(!idx.is_empty(), "索引で引く: {plan}");
        assert!(
            idx.iter().all(|n| n.ends_with("_pkey")),
            "主キーで引く（値索引で列の全行を読まない・generic={generic}）: {idx:?}"
        );
    }
    sqlx::query("DELETE FROM data_index_text WHERE tenant_int = $1")
        .bind(tenant_int)
        .execute(&pool)
        .await
        .unwrap();
}
