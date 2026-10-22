//! 大量データの直接投入（EXPLAIN 回帰とベンチ用）。書込経路を通さず、本体と索引行を SQL で一括生成する
//! （書込経路と同じ正規化: 擬似列は UTC 固定幅 ISO-8601・数値は numeric）。
//!
//! 分布（query-optimizer.md §1.2・§1.3 の形）: n 行のうち
//! - `status`: 承認待ち = g % 3333 == 0（約 0.03%）／未着手 = 新しい 10% の半分（約 5%・新しい行に偏る）／他は 完了
//! - `amount`: 擬似乱数・10% は値なし
//! - `assignee`: `u{g % 10000}`（1 人あたり約 0.01%）
//! - `created_at` / `updated_at`: g の順（新しいほど大きい）
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use authz::AuthContext;
use data::{CmpOp, DataStore, FieldType, PolicyExpr, PolicyOperand, RowPolicy, TableSchema};
use sqlx::PgPool;

use super::{create_table, field, keys_of, schema};

pub const PENDING: &str = "承認待ち";
pub const TODO: &str = "未着手";
pub const DONE: &str = "完了";

/// 業務テーブルのスキーマ（`assignee` の行述語を付けるかどうか）。
pub fn bench_schema(policy: bool) -> TableSchema {
    let mut status = field("status", FieldType::Select);
    status.indexed = true;
    status.options = vec![PENDING.into(), TODO.into(), DONE.into()];
    let mut amount = field("amount", FieldType::Number);
    amount.indexed = true;
    let mut assignee = field("assignee", FieldType::UserRef);
    assignee.indexed = true;
    let mut s = schema(vec![status, amount, assignee]);
    if policy {
        s.row_policy = Some(RowPolicy {
            read: PolicyExpr::FieldCmp {
                field: "assignee".into(),
                op: CmpOp::Eq,
                value: PolicyOperand::UserId,
            },
            write: None,
        });
    }
    s
}

/// テーブルを作り、n 行を直接投入する。`pending_all` なら全行を承認待ちにする（プール統計の雑音用）。
pub async fn seed(
    pool: &PgPool,
    store: &DataStore,
    c: &AuthContext,
    name: &str,
    n: i64,
    policy: bool,
    pending_all: bool,
) -> data::DataTable {
    let table = create_table(store, c, name, bench_schema(policy)).await;
    let (ti, tb) = keys_of(pool, &c.tenant_id, table.id).await;
    let id = |k: &str| i16::try_from(table.schema.field(k).unwrap().id).unwrap();
    let (fs, fa, fu) = (id("status"), id("amount"), id("assignee"));
    let mut tx = pool.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO data_record (id, table_id, table_int, record_int, created_at, updated_at, \
           tenant_int, tenant_id, org, owner, data) \
         SELECT gen_random_uuid(), $1, $2, nextval('data_record_int_seq'), \
                ts, ts, $3, $4, 'acme', 'alice', \
                jsonb_strip_nulls(jsonb_build_object( \
                  'f' || $6::int, st, \
                  'f' || $7::int, CASE WHEN g % 10 = 0 THEN NULL ELSE (g * 7919) % 1000003 END, \
                  'f' || $8::int, 'u' || (g % 10000))) \
         FROM (SELECT g, timestamptz '2026-01-01' + g * interval '1 millisecond' AS ts, \
                 CASE WHEN $9 OR g % 3333 = 0 THEN $10 \
                      WHEN g > $5 * 9 / 10 AND g % 2 = 0 THEN $11 ELSE $12 END AS st \
               FROM generate_series(1, $5::int8) g) s",
    )
    .bind(table.id)
    .bind(tb)
    .bind(ti)
    .bind(&c.tenant_id)
    .bind(n)
    .bind(i32::from(fs))
    .bind(i32::from(fa))
    .bind(i32::from(fu))
    .bind(pending_all)
    .bind(PENDING)
    .bind(TODO)
    .bind(DONE)
    .execute(&mut *tx)
    .await
    .unwrap();
    let iso = "to_char(%s AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')";
    let pseudo = [
        (0i16, "owner".to_string()),
        (1, iso.replace("%s", "created_at")),
        (2, iso.replace("%s", "updated_at")),
    ];
    for (f, expr) in pseudo {
        sqlx::query(&format!(
            "INSERT INTO data_index_text (tenant_int, table_int, field_id, record_int, ord, val, truncated) \
             SELECT tenant_int, table_int, $3, record_int, 0, {expr}, false FROM data_record \
             WHERE tenant_int = $1 AND table_int = $2"
        ))
        .bind(ti)
        .bind(tb)
        .bind(f)
        .execute(&mut *tx)
        .await
        .unwrap();
    }
    for f in [fs, fu] {
        sqlx::query(
            "INSERT INTO data_index_text (tenant_int, table_int, field_id, record_int, ord, val, truncated) \
             SELECT tenant_int, table_int, $3, record_int, 0, data ->> ('f' || $3::int), false \
             FROM data_record WHERE tenant_int = $1 AND table_int = $2 AND data ? ('f' || $3::int)",
        )
        .bind(ti)
        .bind(tb)
        .bind(f)
        .execute(&mut *tx)
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO data_index_num (tenant_int, table_int, field_id, record_int, ord, val) \
         SELECT tenant_int, table_int, $3, record_int, 0, (data ->> ('f' || $3::int))::numeric \
         FROM data_record WHERE tenant_int = $1 AND table_int = $2 AND data ? ('f' || $3::int)",
    )
    .bind(ti)
    .bind(tb)
    .bind(fa)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    for t in ["data_record", "data_index_text", "data_index_num"] {
        sqlx::query(&format!("ANALYZE {t}"))
            .execute(pool)
            .await
            .unwrap();
    }
    table
}
