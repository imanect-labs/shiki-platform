//! 索引テーブルのパーティション（#986）。
//!
//! 索引行の削除（[`super::write`]）は、このテーブルの行が入っているパーティションへ直接向け、組ごとの探索では
//! テナントとテーブルの値を計画時に見せない。値が見えると、統計にこのテーブルの行が無いとき選択率が 0 になり、
//! 主キーと値索引の推定が下限で並んで値索引が選ばれることがあった。値を見せなければ統計の平均の選択率で
//! 見積もられ、4 列等値の主キーが選ばれる。親テーブルに向けたまま値だけ隠すと計画時に刈り込めず、
//! generic plan に固定すると実行時の刈り込みの前に全パーティションをロックするので、どちらも採らない。
//!
//! パーティションに列統計が 1 行も無いと（一度も ANALYZE されていない・空のときに ANALYZE された）平均も無く、
//! 値索引を選んで組ごとにその列の索引行を全部読む。自動 ANALYZE が追いつく前に大量に削除する経路
//! （バックフィル・一括 upsert）は、始める前に [`analyze_index_partitions_without_stats`] で統計を作る。

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use sqlx::PgConnection;
use storage::PoolRouter;

use crate::model::TableKeys;
use crate::{map_db, DataError};

/// 索引行を消す索引テーブル（`data_link` / `data_unique` は別の文で消す）。
const DELETED_PARENTS: [&str; 3] = ["data_index_text", "data_index_num", "data_index_search"];

/// `parent` のうち、このテーブルの行が入っているパーティションの名前（行が無ければ `None`）。
///
/// 同じ `table_int` の行は 1 パーティション（`table_int` のハッシュ）に収まる。
pub(crate) async fn partition_holding(
    pools: &PoolRouter,
    tenant_id: &str,
    parent: &str,
    keys: TableKeys,
) -> Result<Option<String>, sqlx::Error> {
    let rel: Option<String> = sqlx::query_scalar(&format!(
        "SELECT tableoid::regclass::text FROM {parent} \
         WHERE tenant_int = $1 AND table_int = $2 LIMIT 1"
    ))
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .fetch_optional(pools.for_tenant(tenant_id))
    .await?;
    Ok(rel.filter(|r| is_partition_of(r, parent)))
}

/// このテーブルの行が入っているパーティションの番号（`data_record_p{n}` の `n`・行が無ければ `None`）。
///
/// `data_record` と索引テーブルは同じ分割数・同じキー（`table_int`）でハッシュ分割してあり
/// （migration 0085 の `data_create_hash_partitions`）、同じテーブルの行は同じ番号のパーティションに入る。
/// 番号は `table_int` のハッシュで決まって変わらないので、プロセスで覚えておく（`table_int` は採番サービスが
/// プール横断で一意に振る・docs/data/shard-safety.md）。
///
/// 引く文は名前なしの文（`persistent(false)`）で流す。名前付きの文は 6 回目から generic plan になり得て、
/// generic plan は実行時の刈り込みの前に全パーティションとその索引をロックする。
pub(crate) async fn partition_of(
    conn: &mut PgConnection,
    keys: TableKeys,
) -> Result<Option<u16>, DataError> {
    if let Some(n) = cached(keys.table_int) {
        return Ok(Some(n));
    }
    let rel: Option<String> = sqlx::query_scalar(
        "SELECT tableoid::regclass::text FROM data_record \
         WHERE tenant_int = $1 AND table_int = $2 LIMIT 1",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .persistent(false)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db)?;
    let n = rel
        .as_deref()
        .and_then(|r| partition_number(r, "data_record"));
    if let Some(n) = n {
        remember(keys.table_int, n);
    }
    Ok(n)
}

/// `table_int` → パーティション番号。上限を超えたら捨てて引き直す（1 件 10 バイト程度）。
static PARTITIONS: LazyLock<Mutex<HashMap<i64, u16>>> = LazyLock::new(Mutex::default);
const MAX_REMEMBERED: usize = 100_000;

fn cached(table_int: i64) -> Option<u16> {
    PARTITIONS
        .lock()
        .ok()
        .and_then(|m| m.get(&table_int).copied())
}

fn remember(table_int: i64, n: u16) {
    if let Ok(mut m) = PARTITIONS.lock() {
        if m.len() >= MAX_REMEMBERED {
            m.clear();
        }
        m.insert(table_int, n);
    }
}

/// `rel` が `parent` のハッシュパーティション（`{parent}_p{n}`）ならその番号。
pub(crate) fn partition_number(rel: &str, parent: &str) -> Option<u16> {
    let n = rel.strip_prefix(parent)?.strip_prefix("_p")?;
    if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    n.parse().ok()
}

/// `rel` が `parent` かそのハッシュパーティション（`{parent}_p{n}`）の名前か（SQL へ埋め込む前の検証・PIT-21）。
pub(crate) fn is_partition_of(rel: &str, parent: &str) -> bool {
    rel == parent || partition_number(rel, parent).is_some()
}

/// このテーブルの索引行が入っているパーティションのうち、列統計が無いものを ANALYZE する（best-effort）。
///
/// 統計があれば何もしない（1 文で引くだけ）。ANALYZE は読み書きを止めない（`SHARE UPDATE EXCLUSIVE`）。
/// 自動 VACUUM などが同じロックを持っていれば待たずに飛ばす（`SKIP_LOCKED`・次のチャンクでまた確かめる）。
/// 呼び出し元の Tx の外で流す（Tx の中だとロックをコミットまで持ち、自動 VACUUM を待たせる）。
/// 失敗しても書込は止めない（遅くなるだけ）。
#[doc(hidden)]
pub async fn analyze_index_partitions_without_stats(
    pools: &PoolRouter,
    tenant_id: &str,
    tenant_int: i32,
    table_int: i64,
) {
    let holding = DELETED_PARENTS
        .map(|p| {
            format!(
                "(SELECT tableoid AS rel FROM {p} WHERE tenant_int = $1 AND table_int = $2 LIMIT 1)"
            )
        })
        .join(" UNION ALL ");
    let missing: Result<Vec<String>, sqlx::Error> = sqlx::query_scalar(&format!(
        "SELECT p.rel::regclass::text FROM ({holding}) p \
         WHERE NOT EXISTS (SELECT 1 FROM pg_stats s \
           JOIN pg_class c ON c.relname = s.tablename \
           JOIN pg_namespace n ON n.oid = c.relnamespace AND n.nspname = s.schemaname \
          WHERE c.oid = p.rel)"
    ))
    .bind(tenant_int)
    .bind(table_int)
    .fetch_all(pools.for_tenant(tenant_id))
    .await;
    let missing = match missing {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(error = %e, "パーティションの統計を確かめられません");
            return;
        }
    };
    for rel in missing {
        if !DELETED_PARENTS.iter().any(|p| is_partition_of(&rel, p)) {
            continue;
        }
        tracing::info!(rel, "列統計の無いパーティションを ANALYZE します（#986）");
        if let Err(e) = sqlx::query(&format!("ANALYZE (SKIP_LOCKED) {rel}"))
            .execute(pools.for_tenant(tenant_id))
            .await
        {
            tracing::warn!(error = %e, rel, "ANALYZE に失敗（自動 ANALYZE に任せる）");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{is_partition_of, partition_number};

    #[test]
    fn only_partitions_of_the_parent_are_analyzed() {
        assert!(is_partition_of("data_index_num_p12", "data_index_num"));
        assert!(is_partition_of("data_record", "data_record"));
        assert!(!is_partition_of(
            "data_index_num_p1; DROP",
            "data_index_num"
        ));
        assert!(!is_partition_of("data_index_num_p", "data_index_num"));
        assert!(!is_partition_of("data_index_text_p1", "data_index_num"));
    }

    #[test]
    fn partition_number_is_the_suffix_of_the_parent() {
        assert_eq!(partition_number("data_record_p38", "data_record"), Some(38));
        assert_eq!(partition_number("data_record_p0", "data_record"), Some(0));
        assert_eq!(partition_number("data_record", "data_record"), None);
        assert_eq!(partition_number("data_record_p3x", "data_record"), None);
        assert_eq!(partition_number("data_index_text_p3", "data_record"), None);
        assert_eq!(
            partition_number("data_record_p99999", "data_record"),
            None,
            "u16 を超える番号は無い（分割数は 1〜1024）"
        );
    }
}
