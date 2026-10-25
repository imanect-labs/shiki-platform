//! 索引走査の契約（切り詰め列ソートの keyset・切り詰め値の等値の本体再確認）を実 Postgres で固定する。
//! `STORAGE_TEST_DATABASE_URL` が無ければスキップ（Task 15.2 受け入れ条件）。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]

use std::collections::HashSet;

use serde_json::{json, Map, Value};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

use super::*;
use crate::index::state::IndexPlan;
use crate::index::write::{IndexWriter, IndexedRecord};
use crate::index::IndexTable;
use crate::model::{FieldDef, TableSchema};

type Db = sqlx::Pool<sqlx::Postgres>;

async fn setup() -> Option<Db> {
    let Ok(url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let pool = PgPoolOptions::new()
        .max_connections(3)
        .connect(&url)
        .await
        .expect("connect");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    Some(pool)
}

fn schema() -> TableSchema {
    let mut f = FieldDef::new("memo", FieldType::Text);
    f.id = 16;
    f.indexed = true;
    TableSchema {
        fields: vec![f],
        status_field: None,
        row_policy: None,
        field_policy: vec![],
        aggregate_min_rows: None,
        fsm_ref: None,
        rag: None,
    }
}

/// テーブル 1 つとレコード群を直接作り、`memo`（f16）の text 索引をライタで書く。
/// 戻り値は (keys, [(record_int, 値)]) 。
async fn seed(pool: &Db, values: &[String]) -> (TableKeys, Vec<(i64, String)>) {
    let tenant = format!("t-{}", Uuid::new_v4());
    let mut conn = pool.acquire().await.unwrap();
    let placement = storage::tenant::ensure_placement(&mut conn, &tenant)
        .await
        .unwrap();
    let table_int = storage::tenant::allocate_table_int(&mut conn)
        .await
        .unwrap();
    let keys = TableKeys {
        tenant_int: placement.tenant_int,
        table_int,
    };
    let s = schema();
    let table_id: Uuid = sqlx::query_scalar(
        "INSERT INTO data_table (tenant_id, org, name, schema, created_by, tenant_int, table_int, \
           key_format, pivot_index_init) \
         VALUES ($1, 'acme', 'scan', $2, 'alice', $3, $4, 2, true) RETURNING id",
    )
    .bind(&tenant)
    .bind(sqlx::types::Json(&s))
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    let plan = IndexPlan::for_test(keys, &[(16, IndexTable::Text)]);
    let writer = IndexWriter {
        plan: &plan,
        schema: &s,
        tenant_id: &tenant,
        max_multi_values: 50,
    };
    let mut out = Vec::new();
    for v in values {
        let (record_int, created): (i64, chrono::DateTime<chrono::Utc>) = sqlx::query_as(
            "INSERT INTO data_record (tenant_id, table_id, org, data, owner, tenant_int, table_int) \
             VALUES ($1, $2, 'acme', $3, 'alice', $4, $5) RETURNING record_int, created_at",
        )
        .bind(&tenant)
        .bind(table_id)
        .bind(json!({ "f16": v }))
        .bind(keys.tenant_int)
        .bind(keys.table_int)
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        let mut data = Map::new();
        data.insert("memo".into(), Value::String(v.clone()));
        writer
            .on_create(
                &mut conn,
                &IndexedRecord {
                    record_int,
                    owner: "alice",
                    created_at: created,
                    updated_at: created,
                    data: &data,
                },
            )
            .await
            .unwrap();
        out.push((record_int, v.clone()));
    }
    (keys, out)
}

/// 受け入れ条件: 切り詰めが起きる列のソートが §3.3 の契約どおり動く。
/// `(val_prefix, record_int)` の keyset で重複・欠落が無く、同一プレフィクス内は完全値順ではなく
/// record_int 順で、ソート精度は `prefix` と明示される。
#[tokio::test]
async fn truncated_sort_keyset_has_no_duplicates_or_gaps() {
    let Some(pool) = setup().await else { return };
    let common = "a".repeat(MAX_INDEX_KEY_BYTES);
    let mut values = Vec::new();
    // 同一プレフィクス 100 件。接尾辞は**作成順に降順**（完全値順と record_int 順が逆になる）。
    for i in (0..100).rev() {
        values.push(format!("{common}{i:04}"));
    }
    for i in 0..20 {
        values.push(format!("b{i:02}"));
    }
    values.push("0-first".to_string());
    let (keys, seeded) = seed(&pool, &values).await;
    let mut conn = pool.acquire().await.unwrap();

    for descending in [false, true] {
        let mut seen: Vec<i64> = Vec::new();
        let mut after: Option<(String, i64)> = None;
        loop {
            let page = text_sorted_page(
                &mut conn,
                keys,
                16,
                after.as_ref().map(|(v, r)| (v.as_str(), *r)),
                descending,
                7,
            )
            .await
            .unwrap();
            seen.extend(&page.record_ints);
            // カーソルは索引のプレフィクス値（完全値ではない）。
            if let Some((v, _)) = &page.next {
                assert!(v.len() <= MAX_INDEX_KEY_BYTES);
            }
            match page.next {
                Some(n) => after = Some(n),
                None => break,
            }
        }
        let unique: HashSet<i64> = seen.iter().copied().collect();
        assert_eq!(
            unique.len(),
            seen.len(),
            "ページ間の重複が無い（desc={descending}）"
        );
        let all: HashSet<i64> = seeded.iter().map(|(r, _)| *r).collect();
        assert_eq!(unique, all, "欠落が無い（desc={descending}）");

        // 同一プレフィクス群の中は record_int 順（＝決定的な全順序）であって完全値順ではない。
        let group: Vec<i64> = seen
            .iter()
            .copied()
            .filter(|r| {
                seeded
                    .iter()
                    .any(|(sr, v)| sr == r && v.starts_with(&common))
            })
            .collect();
        let mut by_int = group.clone();
        by_int.sort_unstable();
        if descending {
            by_int.reverse();
        }
        assert_eq!(
            group, by_int,
            "同一プレフィクス内は record_int がタイブレーカ"
        );
        let full_order: Vec<i64> = {
            let mut g: Vec<(String, i64)> = seeded
                .iter()
                .filter(|(_, v)| v.starts_with(&common))
                .map(|(r, v)| (v.clone(), *r))
                .collect();
            g.sort();
            if descending {
                g.reverse();
            }
            g.into_iter().map(|(_, r)| r).collect()
        };
        assert_ne!(group, full_order, "完全値順は保証しない（契約どおり）");
    }

    // ソート精度: 切り詰めが起こり得る text 列は prefix、それ以外は exact。
    let s = schema();
    assert_eq!(sort_precision(s.field("memo")), SortPrecision::Prefix);
    assert_eq!(
        serde_json::to_value(sort_precision(s.field("memo"))).unwrap(),
        json!("prefix")
    );
    let sel = FieldDef::new("st", FieldType::Select);
    assert_eq!(sort_precision(Some(&sel)), SortPrecision::Exact);
    assert_eq!(sort_precision(None), SortPrecision::Exact, "擬似列");
}

/// 受け入れ条件: 切り詰め値の等値検索が本体再確認で正しく効く（MAX_TEXT_LEN 上限の値）。
#[tokio::test]
async fn truncated_equality_is_rechecked_against_the_body() {
    let Some(pool) = setup().await else { return };
    let common = "漢".repeat(3_000); // 9,000 バイト（MAX_TEXT_LEN 以内）
    let a = format!("{common}A");
    let b = format!("{common}B");
    let (keys, seeded) = seed(&pool, &[a.clone(), b.clone(), "short".to_string()]).await;
    let mut conn = pool.acquire().await.unwrap();

    // 索引には両方とも同じプレフィクス（truncated）で入っている。
    let truncated: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM data_index_text \
         WHERE tenant_int = $1 AND table_int = $2 AND field_id = 16 AND truncated",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    assert_eq!(truncated, 2);

    let hit_a = text_eq_candidates(&mut conn, keys, 16, "f16", &a)
        .await
        .unwrap();
    assert_eq!(
        hit_a,
        vec![seeded[0].0],
        "プレフィクスが同じ B は本体の再確認で落ちる"
    );
    let hit_b = text_eq_candidates(&mut conn, keys, 16, "f16", &b)
        .await
        .unwrap();
    assert_eq!(hit_b, vec![seeded[1].0]);
    let miss = format!("{common}C");
    assert!(text_eq_candidates(&mut conn, keys, 16, "f16", &miss)
        .await
        .unwrap()
        .is_empty());
    // プレフィクスそのもの（切り詰めていない値）は切り詰め行と一致しない。
    let (prefix, _) = super::super::normalize::prefix_at_char_boundary(&a, MAX_INDEX_KEY_BYTES);
    assert!(text_eq_candidates(&mut conn, keys, 16, "f16", prefix)
        .await
        .unwrap()
        .is_empty());
    let short = text_eq_candidates(&mut conn, keys, 16, "f16", "short")
        .await
        .unwrap();
    assert_eq!(short, vec![seeded[2].0]);
}
