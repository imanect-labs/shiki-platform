//! 消したテーブルの OpenFGA のタプルの撤去（BUG-149）。
//!
//! テーブルの削除は論理削除で、OpenFGA の `data_table` のタプル（持ち主・共有・委譲）は DB の外にある。
//! 残ったタプルは見えるテーブルの数を水増しし、作って消すだけで ListObjects の上限に近づく（一覧は上限に
//! 届いても切れないが、DB の候補を OpenFGA で絞る遅い経路になる・[`crate::table_list`]）。
//!
//! - 削除の commit の後に、その場でタプルを撤去する（[`DataStore::delete_table`]）。撤去を終えた時刻を
//!   `authz_purged_at` に残す（migration 0133）。
//! - 撤去に失敗したもの・この仕組みより前に消したもの（`authz_purged_at` が空のまま）は、
//!   [`DeletedTableTupleSweeper`] が起動時と一定の間隔で古い順に撤去する（全プール・冪等）。掃除でも撤去に失敗した
//!   テーブルは、しばらく対象から外す（失敗し続けるテーブルが 1 回の枠を埋めて、後ろのテーブルの撤去が進まなく
//!   ならないように・サーバーごとの記録）。
//!
//! 消したテーブルは戻せない（論理削除を戻す経路が無い）ので、タプルを撤去しても失うものはない。
//! レコードの個別共有（`data_record` のタプル）は、どのレコードが共有されたかを DB に持たないので、ここでは
//! 撤去しない（テーブルを通さずには読めないので、残っても見えるものは増えない）。

use std::collections::HashMap;
use std::time::{Duration, Instant};

use authz::Namespace;
use uuid::Uuid;

use crate::store::DataStore;
use crate::{map_db, DataError};

/// 掃除 1 回で撤去するテーブル数の上限。
pub const TUPLE_PURGE_BATCH: i64 = 50;
/// 撤去するものが無くなってから、次に見に行くまで（削除の後の撤去に失敗したものを拾う）。
const SWEEP_IDLE: Duration = Duration::from_mins(10);
/// 掃除が失敗したときに、やり直すまで。
const SWEEP_RETRY: Duration = Duration::from_mins(1);
/// 撤去に失敗したテーブルを、次に試すまで（その間は掃除の対象から外し、後ろのテーブルの撤去を進める）。
const FAILED_RETRY_AFTER: Duration = Duration::from_mins(30);
/// 後回しにするテーブルの数の上限（覚えきれない分は、古い順に掃除の対象へ戻す）。
const MAX_DEFERRED: usize = 10_000;

/// 掃除 1 回の結果。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TuplePurgeProgress {
    /// タプルを撤去して印を付けたテーブル数。
    pub purged: u64,
    /// 撤去に失敗したテーブル数（印を付けず、次の掃除でやり直す）。
    pub failed: u64,
}

impl DataStore {
    /// 消したテーブルの `data_table` のタプルを撤去し、撤去を終えた印を付ける（冪等）。
    ///
    /// 削除の commit の後に呼ぶ。失敗しても削除は取り消さない（掃除がやり直す）。
    pub(crate) async fn purge_deleted_table_tuples_of(&self, tenant_id: &str, id: Uuid) {
        let object = Namespace::for_tenant(tenant_id).data_table(&id.to_string());
        if let Err(e) = self.authz.delete_object_tuples(&object).await {
            tracing::warn!(error = %e, table_id = %id, "消したテーブルのタプルの撤去に失敗（掃除がやり直す）");
            return;
        }
        let marked = sqlx::query(
            "UPDATE data_table SET authz_purged_at = now() \
             WHERE tenant_id = $1 AND id = $2 AND deleted_at IS NOT NULL",
        )
        .bind(tenant_id)
        .bind(id)
        .execute(self.pools.for_tenant(tenant_id))
        .await;
        if let Err(e) = marked {
            tracing::warn!(error = %e, table_id = %id, "タプルの撤去の印を付けられなかった（掃除が撤去し直す）");
        }
    }

    /// 消したのにタプルを撤去していないテーブルを、消した順に最大 [`TUPLE_PURGE_BATCH`] 件撤去する（全プール）。
    ///
    /// 対象の行を `FOR UPDATE SKIP LOCKED` で押さえる（複数のサーバーが同時に掃除しても同じテーブルを
    /// 二度撤去しない。撤去は冪等なので、重なっても壊れはしない）。
    pub async fn purge_deleted_table_tuples(&self) -> Result<TuplePurgeProgress, DataError> {
        Ok(self.purge_deleted_table_tuples_except(&[]).await?.0)
    }

    /// [`Self::purge_deleted_table_tuples`] の、`skip` のテーブルを対象から外すもの。撤去に失敗したテーブルの id も
    /// 返す（掃除はそれをしばらく後回しにし、撤去に失敗し続けるテーブルが先頭の枠を埋めて後ろが進まないのを防ぐ）。
    pub async fn purge_deleted_table_tuples_except(
        &self,
        skip: &[Uuid],
    ) -> Result<(TuplePurgeProgress, Vec<Uuid>), DataError> {
        let mut progress = TuplePurgeProgress::default();
        let mut failed_ids = Vec::new();
        let mut budget = TUPLE_PURGE_BATCH;
        for (_, pool) in self.pools.all() {
            if budget <= 0 {
                break;
            }
            let mut tx = pool.begin().await.map_err(map_db)?;
            // shard-safety: allow(cross-tenant) 保守スイープ。プールごとに問い合わせる（合算は Rust 側）。
            let targets: Vec<(String, Uuid)> = sqlx::query_as(
                "SELECT tenant_id, id FROM data_table \
                 WHERE deleted_at IS NOT NULL AND authz_purged_at IS NULL \
                   AND NOT (id = ANY($2)) \
                 ORDER BY deleted_at LIMIT $1 FOR UPDATE SKIP LOCKED",
            )
            .bind(budget)
            .bind(skip)
            .fetch_all(&mut *tx)
            .await
            .map_err(map_db)?;
            for (tenant_id, id) in targets {
                budget -= 1;
                let object = Namespace::for_tenant(&tenant_id).data_table(&id.to_string());
                match self.authz.delete_object_tuples(&object).await {
                    Ok(_) => {
                        sqlx::query(
                            "UPDATE data_table SET authz_purged_at = now() \
                             WHERE tenant_id = $1 AND id = $2",
                        )
                        .bind(&tenant_id)
                        .bind(id)
                        .execute(&mut *tx)
                        .await
                        .map_err(map_db)?;
                        progress.purged += 1;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, table_id = %id, "消したテーブルのタプルの撤去に失敗（しばらく後回しにしてやり直す）");
                        progress.failed += 1;
                        failed_ids.push(id);
                    }
                }
            }
            tx.commit().await.map_err(map_db)?;
        }
        Ok((progress, failed_ids))
    }
}

/// 撤去に失敗したテーブルを後回しにする記録（掃除の常駐ワーカーが持つ・サーバーごと）。
#[derive(Debug, Default)]
struct Deferred {
    /// テーブルの id と、次に試してよい時刻。
    until: HashMap<Uuid, Instant>,
}

impl Deferred {
    /// いま外すテーブル（試してよい時刻を過ぎたものは忘れ、掃除の対象へ戻す）。
    fn skip(&mut self, now: Instant) -> Vec<Uuid> {
        self.until.retain(|_, until| *until > now);
        self.until.keys().copied().collect()
    }

    /// 撤去に失敗したテーブルを、[`FAILED_RETRY_AFTER`] の間は後回しにする（上限を超える分は覚えない）。
    fn record(&mut self, failed: &[Uuid], now: Instant) {
        for id in failed {
            if self.until.len() >= MAX_DEFERRED {
                break;
            }
            self.until.insert(*id, now + FAILED_RETRY_AFTER);
        }
    }
}

/// 消したテーブルのタプルの掃除の常駐ワーカー（起動時に溜まった分を撤去し、その後は一定の間隔で見る）。
#[derive(Clone)]
pub struct DeletedTableTupleSweeper {
    store: DataStore,
}

impl DeletedTableTupleSweeper {
    pub fn new(store: DataStore) -> Self {
        DeletedTableTupleSweeper { store }
    }

    /// バックグラウンドで回す。
    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move { self.run().await })
    }

    async fn run(self) {
        let mut deferred = Deferred::default();
        loop {
            let skip = deferred.skip(Instant::now());
            let wait = match self.store.purge_deleted_table_tuples_except(&skip).await {
                Ok((p, failed)) => {
                    deferred.record(&failed, Instant::now());
                    if p.purged > 0 || p.failed > 0 {
                        tracing::info!(
                            purged = p.purged,
                            failed = p.failed,
                            "消したテーブルのタプルを撤去しました"
                        );
                    }
                    // 1 回分を撤去しきったら、続きがあるかもしれないのですぐ見に行く（失敗だけのときは待つ）。
                    if p.purged > 0 && p.purged + p.failed >= TUPLE_PURGE_BATCH.unsigned_abs() {
                        Duration::ZERO
                    } else if p.failed > 0 {
                        SWEEP_RETRY
                    } else {
                        SWEEP_IDLE
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "消したテーブルのタプルの掃除に失敗");
                    SWEEP_RETRY
                }
            };
            tokio::time::sleep(wait).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 撤去に失敗したテーブルは、しばらく掃除の対象から外し、時刻を過ぎたら戻す。
    #[test]
    fn failed_tables_are_deferred_then_retried() {
        let mut d = Deferred::default();
        let t0 = Instant::now();
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        assert!(d.skip(t0).is_empty());
        d.record(&[a, b], t0);
        let mut skipped = d.skip(t0 + Duration::from_mins(1));
        skipped.sort();
        assert_eq!(skipped, vec![a, b]);
        assert!(d
            .skip(t0 + FAILED_RETRY_AFTER + Duration::from_secs(1))
            .is_empty());
    }
}
