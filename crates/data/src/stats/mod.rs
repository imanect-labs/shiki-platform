//! 自前オプティマイザの統計の**書き手**（query-optimizer.md §4・Task 15.12）。
//!
//! | 統計 | 書き手 | 表 |
//! |---|---|---|
//! | 行数・値頻度表 | outbox の消費側（[`counters`]・台帳 consumer [`STATS_CONSUMER`]） | `data_table_stats` / `data_value_count` |
//! | フィールド統計・リンク統計・行幅 | 統計収集ジョブ（[`analyze`]・jobq [`STATS_ANALYZE_QUEUE`]） | `data_field_stats` / `data_link_stats` / `data_table_stats` |
//!
//! - **値頻度表は書込 Tx で更新しない**（人気の値の 1 行に書込が集中する）。outbox の変更の非公開部
//!   （`counts.b` / `counts.a`・[`crate::stream::outbox`]）から前後の値を読み、消費側がバッチで
//!   `cnt = cnt + excluded.cnt` する。冪等性は台帳 consumer の配送記録（同じ Tx で `mark_delivered`）で担保する。
//! - **統計は可視性を無視して全行から集める**（PIT-75）。ここは書くだけで、統計値を呼び出し側へ返す関数を
//!   持たない（読むのは自分の制御列＝数え直しの要否・対象列・スキップする変更だけ）。統計の読取経路は
//!   `crate::query::planner` に閉じる（tests/shard_safety_lint.rs の PIT-75 検査）。

pub(crate) mod analyze;
pub(crate) mod counters;

use std::time::Duration;

use crate::field_key::json_key;
use crate::model::{FieldType, TableSchema};
use crate::store::DataStore;

/// 統計の台帳 consumer（outbox の全イベントを 1 度ずつ見る・GC はこの配送を待つ）。
pub const STATS_CONSUMER: &str = "data-stats";
/// 統計収集ジョブのキュー。
pub const STATS_ANALYZE_QUEUE: &str = "data_stats_analyze";
/// 値頻度表を持つ列の種類数の上限（超えた列は値頻度表から外し、フィールド統計で推定する）。
pub const VALUE_COUNT_MAX_DISTINCT: i64 = 1_024;
/// フィールド統計を全件で作る索引行数の上限（超えたら行数に比例して間引く・§4.2）。
pub(crate) const ANALYZE_FULL_SCAN_MAX: i64 = 1_000_000;

/// 統計収集の起動条件（autoanalyze と同じ形）: 前回からの変更行数 > 50 + 0.1 × 行数。
pub(crate) fn analyze_due(changes: i64, n_rows: i64) -> bool {
    changes > 50 + n_rows.max(0) / 10
}

/// 値頻度表の対象になり得る列（索引済みの分類列: select / multi_select / user_ref / role_ref）。
/// owner 擬似列（field 0）は常に対象。実際に数えるかは種類数で決まる（[`VALUE_COUNT_MAX_DISTINCT`]）。
pub(crate) fn counts_eligible(f: &crate::model::FieldDef) -> bool {
    (f.indexed || f.unique)
        && matches!(
            f.field_type,
            FieldType::Select | FieldType::MultiSelect | FieldType::UserRef | FieldType::RoleRef
        )
}

/// outbox の非公開部に前後の値を載せる列の保存形キー（`"f{id}"`）。
pub(crate) fn counts_keys(schema: &TableSchema) -> Vec<String> {
    schema
        .fields
        .iter()
        .filter(|f| counts_eligible(f))
        .map(|f| json_key(f.id))
        .collect()
}

/// 統計の常駐ワーカー（outbox の消費 → 統計収集ジョブ 1 件 → 待機）。
///
/// 統計収集はテナント間で公平な取り出し（[`jobq::claim`] の fair_seq 順）で、プロセスあたり同時に 1 本。
/// 1 列ごとに処理時間と同じだけ休む（負荷を半分以下に抑える）ので、重いテナントの統計収集が
/// 他テナントの読取を圧迫しない。
#[derive(Clone)]
pub struct StatsWorker {
    store: DataStore,
    poll: Duration,
}

impl StatsWorker {
    pub fn new(store: DataStore) -> Self {
        StatsWorker {
            store,
            poll: Duration::from_secs(2),
        }
    }

    /// バックグラウンドで回す（台帳 consumer の登録 → 消費と統計収集の繰り返し）。
    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            if let Err(e) = self.store.register_stats_consumer().await {
                tracing::error!(error = %e, "data-stats consumer の登録に失敗");
            }
            loop {
                let mut busy = false;
                match self.store.consume_stats_once().await {
                    Ok(n) => busy |= n > 0,
                    Err(e) => {
                        tracing::warn!(error = %e, "統計の消費に失敗（次の周期で再試行）");
                    }
                }
                match self.store.run_stats_job_once().await {
                    Ok(ran) => busy |= ran,
                    Err(e) => {
                        tracing::warn!(error = %e, "統計収集ジョブに失敗（jobq がバックオフ再配信）");
                    }
                }
                if !busy {
                    tokio::time::sleep(self.poll).await;
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::FieldDef;

    #[test]
    fn analyze_trigger_follows_autoanalyze_shape() {
        assert!(!analyze_due(50, 0));
        assert!(analyze_due(51, 0));
        assert!(!analyze_due(150, 1_000));
        assert!(analyze_due(151, 1_000));
    }

    #[test]
    fn only_indexed_categorical_columns_are_counted() {
        let mut s = FieldDef::new("status", FieldType::Select);
        assert!(!counts_eligible(&s), "未索引は数えない");
        s.indexed = true;
        assert!(counts_eligible(&s));
        let mut t = FieldDef::new("title", FieldType::Text);
        t.indexed = true;
        assert!(!counts_eligible(&t), "自由文は数えない");
        let mut u = FieldDef::new("who", FieldType::UserRef);
        u.unique = true;
        assert!(counts_eligible(&u));
    }
}
