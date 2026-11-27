//! gui の結合テストの後片付け（テストで作った行を最後に消す・BUG-63/66 の教訓）。

use sqlx::PgPool;

/// テストのテナントの行を、テナント撤去（`StorageService::purge_tenant` の DB 段）と同じ範囲で消す。
///
/// 撤去と同じく、監査ログ・テナント台帳・`tenant_int` の台帳（`PURGE_RETAINED`）は残す。OpenFGA は
/// テストごとのストアなので触れない。消した後に、残すもの以外の行が無いことを確かめる。
pub(crate) async fn purge_tenant_rows(pool: &PgPool, tenant: &str) {
    use storage::tenant_scope::{
        count_tenant_rows, delete_tenant_int_rows, tenant_int_scoped_tables, tenant_scoped_tables,
        PURGE_RETAINED,
    };
    let retained = |table: &str| PURGE_RETAINED.iter().any(|(t, _)| *t == table);
    let tables = tenant_scoped_tables(pool).await.expect("テナントの表");
    let int_tables = tenant_int_scoped_tables(pool)
        .await
        .expect("tenant_int の表");
    let mut tx = pool.begin().await.expect("tx");
    for table in tables.iter().filter(|t| !retained(t)) {
        sqlx::query(&format!("DELETE FROM public.{table} WHERE tenant_id = $1"))
            .bind(tenant)
            .execute(&mut *tx)
            .await
            .unwrap_or_else(|e| panic!("{table} の後片付け: {e}"));
    }
    delete_tenant_int_rows(&mut tx, &int_tables, tenant)
        .await
        .expect("tenant_int の表の後片付け");
    tx.commit().await.expect("後片付けの commit");
    let left: Vec<(String, i64)> = count_tenant_rows(pool, &tables, tenant)
        .await
        .expect("残りの数")
        .into_iter()
        .filter(|(t, n)| *n > 0 && !retained(t))
        .collect();
    assert!(
        left.is_empty(),
        "テストのテナントに行が残っている: {left:?}"
    );
}
