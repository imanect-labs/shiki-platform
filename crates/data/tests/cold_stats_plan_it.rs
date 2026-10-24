//! 取込直後（ANALYZE 前・統計なし）の「範囲 ＋ 同じ列の並べ替え」が値索引から範囲の端へシークする
//! （検証 2-D-P2-1）。実 Postgres（`STORAGE_TEST_DATABASE_URL`・統計の消去に superuser が要る）で実行する。
//!
//! 取込直後のパーティションには Postgres の統計が無く、候補集め（`cand`）が値索引
//! `(tenant_int, table_int, field_id, val, record_int)` を使わずに範囲の外を読み飛ばしていた
//! （10 万行で 1.4〜1.6 秒）。索引表の統計を消して「取込直後」を作り、数値・日付・テキストの各列で
//! 自然な選択（強制なし）の文を EXPLAIN ANALYZE し、索引表を全走査しないこと・値索引から入り読む行数が
//! 範囲の大きさで抑えられることを固定する。テスト内の並行を避けるため 1 ファイル 1 テスト。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::many_single_char_names
)]

mod common;

use common::{
    create_table, ctx, explain, field, keys_of, schema, setup, store_with, unique_tenant,
};
use data::{CondOp, Condition, DataQuery, FieldType, ForcedStrategy, QueryTuning, SortKey};
use serde_json::{json, Value};

const N: i64 = 30_000;

/// 計画中の走査ノード（種類・対象・索引・実測行数）。
fn scans(plan: &Value) -> Vec<(String, String, String, i64)> {
    fn walk(v: &Value, out: &mut Vec<(String, String, String, i64)>) {
        match v {
            Value::Object(m) => {
                let nt = m.get("Node Type").and_then(Value::as_str).unwrap_or("");
                let idx = m.get("Index Name").and_then(Value::as_str).unwrap_or("");
                // Bitmap Index Scan は対象表の名前を持たないので索引名の接頭辞（`<表>_tenant_int_…`）から取る。
                let rel = m
                    .get("Relation Name")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| {
                        idx.split("_tenant_int")
                            .next()
                            .filter(|_| !idx.is_empty())
                            .map(str::to_string)
                    });
                if let Some(rel) = rel {
                    out.push((
                        nt.to_string(),
                        rel,
                        idx.to_string(),
                        m.get("Actual Rows").and_then(Value::as_i64).unwrap_or(-1)
                            * m.get("Actual Loops").and_then(Value::as_i64).unwrap_or(1),
                    ));
                }
                m.values().for_each(|x| walk(x, out));
            }
            Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(plan, &mut out);
    out
}

#[tokio::test]
async fn range_and_same_column_sort_seek_the_value_index_before_analyze() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let mut amount = field("amount", FieldType::Number);
    amount.indexed = true;
    let mut due = field("due", FieldType::Date);
    due.indexed = true;
    let mut code = field("code", FieldType::Text);
    code.indexed = true;
    let table = create_table(&store, &c, "cold", schema(vec![amount, due, code])).await;
    let (ti, tb) = keys_of(&pool, &tenant, table.id).await;
    let id = |k: &str| i32::from(i16::try_from(table.schema.field(k).unwrap().id).unwrap());
    let (fa, fd, fc) = (id("amount"), id("due"), id("code"));
    // 本体と索引行を直接投入する（取込の適用と同じ形・ANALYZE はしない）。
    sqlx::query(
        "INSERT INTO data_record (id, table_id, table_int, record_int, created_at, updated_at, \
           tenant_int, tenant_id, org, owner, data) \
         SELECT gen_random_uuid(), $1, $2, nextval('data_record_int_seq'), now(), now(), $3, $4, \
                'acme', 'alice', jsonb_build_object( \
                  'f' || $6::int, (g * 7919) % 1000003, \
                  'f' || $7::int, to_char(date '2020-01-01' + (g % 3000)::int, 'YYYY-MM-DD'), \
                  'f' || $8::int, 'c' || lpad(((g * 31) % 100000)::text, 6, '0')) \
         FROM generate_series(1, $5::int8) g",
    )
    .bind(table.id)
    .bind(tb)
    .bind(ti)
    .bind(&tenant)
    .bind(N)
    .bind(fa)
    .bind(fd)
    .bind(fc)
    .execute(&pool)
    .await
    .unwrap();
    for (f, ty) in [(0i32, "owner"), (1, "created_at"), (2, "updated_at")] {
        let expr = if ty == "owner" {
            "owner".to_string()
        } else {
            format!("to_char({ty} AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')")
        };
        sqlx::query(&format!(
            "INSERT INTO data_index_text (tenant_int, table_int, field_id, record_int, ord, val, truncated) \
             SELECT tenant_int, table_int, $3::int, record_int, 0, {expr}, false FROM data_record \
             WHERE tenant_int = $1 AND table_int = $2"
        ))
        .bind(ti)
        .bind(tb)
        .bind(f)
        .execute(&pool)
        .await
        .unwrap();
    }
    for f in [fd, fc] {
        sqlx::query(
            "INSERT INTO data_index_text (tenant_int, table_int, field_id, record_int, ord, val, truncated) \
             SELECT tenant_int, table_int, $3::int, record_int, 0, data ->> ('f' || $3::int), false \
             FROM data_record WHERE tenant_int = $1 AND table_int = $2",
        )
        .bind(ti)
        .bind(tb)
        .bind(f)
        .execute(&pool)
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO data_index_num (tenant_int, table_int, field_id, record_int, ord, val) \
         SELECT tenant_int, table_int, $3::int, record_int, 0, (data ->> ('f' || $3::int))::numeric \
         FROM data_record WHERE tenant_int = $1 AND table_int = $2",
    )
    .bind(ti)
    .bind(tb)
    .bind(fa)
    .execute(&pool)
    .await
    .unwrap();

    // 「取込直後」の 2 つの形: ① 統計が無い（一度も ANALYZE されていない新しいパーティション）
    // ② 空に近いときの統計のまま（取込の前に ANALYZE された・検証で遅かった形）。
    for stale in [false, true] {
        for parent in ["data_index_num", "data_index_text", "data_record"] {
            let rels = format!(
                "(SELECT inhrelid FROM pg_inherits WHERE inhparent = '{parent}'::regclass \
                  UNION SELECT '{parent}'::regclass)"
            );
            if stale {
                // 列の統計はあるが、行数・ページ数は空に近いときのまま。
                sqlx::query(&format!("ANALYZE {parent}"))
                    .execute(&pool)
                    .await
                    .unwrap();
            } else {
                sqlx::query(&format!(
                    "DELETE FROM pg_statistic WHERE starelid IN {rels}"
                ))
                .execute(&pool)
                .await
                .unwrap();
            }
            let set = if stale {
                "reltuples = 0, relpages = 1, relallvisible = 0"
            } else {
                "reltuples = -1, relpages = 0, relallvisible = 0"
            };
            sqlx::query(&format!("UPDATE pg_class SET {set} WHERE oid IN {rels}"))
                .execute(&pool)
                .await
                .unwrap();
        }
        // 空に近いときの統計のまま（取込の完了時の ANALYZE の前）は、自然な選択（ソート駆動）だけを見る。
        check_all(&store, &pool, &c, table.id, stale).await;
    }
    // 共有のテスト DB の統計を戻す（他のテストの計画に影響させない）。
    for parent in ["data_index_num", "data_index_text", "data_record"] {
        sqlx::query(&format!("ANALYZE {parent}"))
            .execute(&pool)
            .await
            .unwrap();
    }
}

async fn check_all(
    store: &data::DataStore,
    pool: &sqlx::PgPool,
    c: &authz::AuthContext,
    table_id: uuid::Uuid,
    natural_only: bool,
) {
    let range = |f: &str, lo: Value, hi: Value| {
        Some(Condition::All(vec![
            Condition::cmp(f, CondOp::Gte, Some(lo)),
            Condition::cmp(f, CondOp::Lt, Some(hi)),
        ]))
    };
    // 各列で範囲（全体の約 1%）＋同じ列の昇順・降順。
    let cases: [(&str, Value, Value, &str); 3] = [
        ("amount", json!(500_000), json!(510_000), "data_index_num"),
        (
            "due",
            json!("2021-01-01"),
            json!("2021-01-31"),
            "data_index_text",
        ),
        (
            "code",
            json!("c050000"),
            json!("c051000"),
            "data_index_text",
        ),
    ];
    // 自然な選択と、検証で遅かった候補駆動（フィルタ駆動）の両方。
    let mut tunings = vec![QueryTuning::default()];
    if !natural_only {
        tunings.push(QueryTuning {
            force: Some(ForcedStrategy::Filter),
            ..QueryTuning::default()
        });
    }
    for ((f, lo, hi, rel), tuning) in cases
        .iter()
        .flat_map(|c| tunings.iter().map(move |t| (c.clone(), t)))
    {
        let (lo, hi) = (lo.clone(), hi.clone());
        for desc in [false, true] {
            let q = DataQuery {
                filter: range(f, lo.clone(), hi.clone()),
                sort: vec![SortKey {
                    field: f.into(),
                    descending: desc,
                }],
                limit: Some(50),
                ..Default::default()
            };
            let (r, d) = store
                .run_query_debug(c, table_id, &q, tuning)
                .await
                .unwrap();
            let items = r.items.unwrap();
            assert_eq!(items.len(), 50, "{f}");
            // 結果は範囲の中で正しい順。
            let vals: Vec<String> = items
                .iter()
                .map(|x| x.data[f].to_string().trim_matches('"').to_string())
                .collect();
            if f == "amount" {
                let n: Vec<i64> = vals.iter().map(|v| v.parse().unwrap()).collect();
                assert!(n.iter().all(|v| (500_000..510_000).contains(v)), "{n:?}");
                assert!(n
                    .windows(2)
                    .all(|w| if desc { w[0] >= w[1] } else { w[0] <= w[1] }));
            } else {
                let (lo, hi) = (lo.as_str().unwrap(), hi.as_str().unwrap());
                assert!(
                    vals.iter().all(|v| v.as_str() >= lo && v.as_str() < hi),
                    "{vals:?}"
                );
                assert!(vals
                    .windows(2)
                    .all(|w| if desc { w[0] >= w[1] } else { w[0] <= w[1] }));
            }
            let mut seen_val_idx = false;
            for st in &d.statements {
                let plan = explain(pool, st, true).await;
                for (nt, r, idx, rows) in scans(&plan) {
                    if !r.starts_with(&format!("{rel}_p")) && !r.starts_with("data_index_") {
                        continue;
                    }
                    assert!(
                        !nt.contains("Seq Scan"),
                        "{f} desc={desc} {}: 索引表を全走査した {r}: {plan}",
                        st.label
                    );
                    // 範囲は約 300 行。どの索引でも範囲の外を読み飛ばさない（主キーで列の全行 30,000 を
                    // 拾って値で落とす計画にしない）。
                    assert!(
                        rows <= 2_000,
                        "{f} desc={desc} {}: {idx} で {rows} 行読んだ: {plan}",
                        st.label
                    );
                    if r.starts_with(&format!("{rel}_p")) && idx.contains("_val_") {
                        seen_val_idx = true;
                    }
                }
            }
            assert!(seen_val_idx, "{f} desc={desc}: 値索引から入る");
        }
    }
}
