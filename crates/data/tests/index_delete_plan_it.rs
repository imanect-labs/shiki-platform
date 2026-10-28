//! 索引行の削除文の計画の形（検証 D-6・#915・実 PG）。
//!
//! バックフィルは 1 チャンクごとに (列, レコード) の組の索引行を消してから作り直す。新しい列は統計に
//! 載っていないため、`field_id = ANY … AND record_int = ANY …` の形では Postgres が値索引を選び、配列の
//! 要素ごとにその列の索引行を全部読み直した（30 万行で 1 回 3.5 秒・バックフィル 15 分）。`unnest` との
//! 結合に開いた形でも、統計の具合でハッシュ結合＋3 列の前方一致（列の全行を読む）になり、索引も主キーと
//! 値索引の間で入れ替わった（#915）。削除文が統計の古さに関わらず主キーの 4 列等値で組ごとに引くことを
//! `EXPLAIN` で固定し、実際に組の行だけが消えることも確かめる。
//!
//! 統計（ANALYZE）を書き換えるので、他のテストと計画が干渉しないよう独立したテストバイナリにする。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use serde_json::Value;

/// 計画中の索引探索（索引名・Index Cond）を集める。
fn index_scans(plan: &Value, out: &mut Vec<(String, String)>) {
    match plan {
        Value::Object(m) => {
            if let Some(Value::String(n)) = m.get("Index Name") {
                let cond = m
                    .get("Index Cond")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                out.push((n.clone(), cond.to_string()));
            }
            for v in m.values() {
                index_scans(v, out);
            }
        }
        Value::Array(a) => a.iter().for_each(|v| index_scans(v, out)),
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
    let fill = |lo: i64, hi: i64, f0: i32, f1: i32| {
        sqlx::query(insert)
            .bind(tenant_int)
            .bind(table_int)
            .bind(lo)
            .bind(hi)
            .bind(f0)
            .bind(f1)
            .execute(&pool)
    };
    fill(1, 300, 16, 18).await.unwrap();
    sqlx::query("ANALYZE data_index_text")
        .execute(&pool)
        .await
        .unwrap();
    fill(301, 100_000, 16, 18).await.unwrap();
    fill(1, 60_000, 19, 19).await.unwrap();

    let records: Vec<i64> = (59_501..=60_500).collect();
    let fields: Vec<i16> = vec![16, 19];
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
        let mut scans = Vec::new();
        index_scans(&plan, &mut scans);
        assert!(!scans.is_empty(), "索引で引く: {plan}");
        for (name, cond) in &scans {
            assert!(
                name.ends_with("_pkey") && cond.contains("record_int ="),
                "主キーの 4 列等値で組ごとに引く（列の全行を読まない・generic={generic}）: {scans:?}"
            );
        }
    }

    // 実際に消えるのは組の行だけ（列 16・19 × 1,000 件のうち、列 19 は 60,000 までしか無い）。
    let deleted = sqlx::query(&data::text_index_delete_sql())
        .bind(tenant_int)
        .bind(table_int)
        .bind(&fields)
        .bind(&records)
        .execute(&pool)
        .await
        .unwrap()
        .rows_affected();
    assert_eq!(deleted, 1_000 + 500);
    let left: Vec<(i16, i64)> = sqlx::query_as(
        "SELECT field_id, count(*) FROM data_index_text \
         WHERE tenant_int = $1 AND table_int = $2 AND record_int BETWEEN 59501 AND 60500 \
         GROUP BY field_id ORDER BY field_id",
    )
    .bind(tenant_int)
    .bind(table_int)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(left, vec![(17, 1_000), (18, 1_000)], "組に無い列は残る");

    sqlx::query("DELETE FROM data_index_text WHERE tenant_int = $1")
        .bind(tenant_int)
        .execute(&pool)
        .await
        .unwrap();
}
