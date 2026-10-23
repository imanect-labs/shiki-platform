//! 索引のオンラインバックフィル（jobq のチャンクジョブ・data-platform §10・§13 M2・PIT-72）。
//!
//! - **初期化**: Task 15.2 より前に作られたテーブルは台帳が無い（`data_table.pivot_index_init = false`）。
//!   初回アクセス（[`DataStore::fetch_live`]）か常駐ワーカーのスイープが [`DataStore::init_pivot_index`] で
//!   台帳を作り（既存レコードがあれば `building`）、unique を同期構築し、ジョブを積む。
//! - **スキーマ改訂**で索引が増えたときも同じく `building` にしてジョブを積む（[`start_backfill`]）。
//! - **チャンク**（[`DataStore::index_backfill_step`]）: 進捗行を `FOR UPDATE` で押さえ、`record_int` 順に
//!   `FOR SHARE` で最大 N 件読み、`building` の組の索引行を本体から作り直して進捗を進める。1 チャンク =
//!   1 Tx なので**冪等・中断再開可**（落ちたチャンクはロールバックされ、次回同じ位置から）。
//! - 走査が開始時点の最大 `record_int`（high_water）に達したら、同じ Tx で `building` を**原子的に**
//!   `active` へ切り替え、parity 検証ジョブを積む（[`super::parity`]）。開始後に作られたレコードは
//!   書込経路が write-through 済み（[`super::state`] の競合しない理由）。
//! - 進捗（件数・状態・parity 結果）は [`DataStore::index_backfill_status`] で観測できる。
//!   `record_int`（内部キー）は返さない（PIT-75）。

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use sqlx::types::Json;
use sqlx::PgConnection;
use uuid::Uuid;

use super::state::{activate_building, load_states, sync_ledger, IndexPlan};
use super::write::{IndexWriter, IndexedRecord};
use super::{IndexState, IndexTable, MAX_MULTI_VALUES_CEILING};
use crate::field_key::decode_record;
use crate::key_migration::KEY_FORMAT_LEGACY;
use crate::link_values::{LINK_VALUES_OP, LINK_VALUE_CHUNK};
use crate::model::{FieldId, TableKeys, TableSchema};
use crate::store::DataStore;
use crate::{map_db, DataError};

/// バックフィル／parity 検証ジョブのキュー名。
pub const INDEX_BACKFILL_QUEUE: &str = "data_index_backfill";
/// 1 チャンク（1 Tx）で作り直すレコード数。
pub const BACKFILL_CHUNK: i64 = 500;
/// ジョブ 1 件で処理するチャンク数（残りは続きのジョブへ）。
const CHUNKS_PER_JOB: usize = 40;
/// 可視タイムアウト（1 ジョブの想定所要より十分長く）。
const BACKFILL_VT: Duration = Duration::from_mins(15);
/// スイープ 1 回で初期化するテーブル数の上限。
const INIT_SWEEP_BATCH: i64 = 50;

/// チャンク 1 回の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexBackfillStep {
    /// このチャンクで作り直したレコード数。
    pub processed: u64,
    /// 走査が終わり `building` を `active` へ切り替えた（または対象が無かった）。
    pub done: bool,
}

/// バックフィルの進捗（運用の観測用・内部キーを含まない）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexBackfillStatus {
    /// `running` / `done` / `failed`。
    pub status: String,
    /// 作り直し済みのレコード数。
    pub rows_done: i64,
    /// 開始時点のレコード数（ETA の分母）。
    pub rows_total: i64,
    /// 開始のたびに増える世代（スキーマ改訂で索引が増えると再開始する）。
    pub generation: i64,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    /// 最後に parity 検証した時刻と不一致件数（0 で一致）。
    pub parity_checked_at: Option<DateTime<Utc>>,
    pub parity_mismatches: Option<i32>,
    pub last_error: Option<String>,
}

/// 進捗をリセットしてバックフィルを（再）開始し、ジョブを積む（`data_table` 行を `FOR UPDATE` 済みの Tx 上）。
///
/// 走査中に別の索引が増えた場合もここで先頭からやり直す（作り直しは冪等なので、走査済みの
/// レコードを二度処理しても結果は同じ）。
pub(crate) async fn start_backfill(
    conn: &mut PgConnection,
    keys: TableKeys,
    tenant_id: &str,
    table_id: Uuid,
) -> Result<(), DataError> {
    start_backfill_from(conn, keys, tenant_id, table_id, 0).await
}

/// [`start_backfill`] の開始位置つき版（`record_int > from` だけを走査する）。
///
/// 一括取込の索引の遅延構築（[`crate::bulk`]）が、取込で入れた範囲だけを作り直すのに使う。
/// `from` より前のレコードの `building` の組が既に揃っている（＝取込が `active` から `building` に
/// 落とした組だけが残っている）ことは呼び出し側が保証する。
pub(crate) async fn start_backfill_from(
    conn: &mut PgConnection,
    keys: TableKeys,
    tenant_id: &str,
    table_id: Uuid,
    from: i64,
) -> Result<(), DataError> {
    let (total, high): (i64, i64) = sqlx::query_as(
        "SELECT count(*), coalesce(max(record_int), 0) FROM data_record \
         WHERE tenant_int = $1 AND table_int = $2 AND record_int > $3",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .bind(from)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db)?;
    sqlx::query(
        "INSERT INTO data_index_backfill \
           (tenant_int, table_int, cursor_record, high_water, rows_done, rows_total, status) \
         VALUES ($1, $2, $5, $3, 0, $4, 'running') \
         ON CONFLICT (tenant_int, table_int) DO UPDATE SET \
           cursor_record = excluded.cursor_record, high_water = excluded.high_water, rows_done = 0, \
           rows_total = excluded.rows_total, status = 'running', \
           generation = data_index_backfill.generation + 1, started_at = now(), \
           updated_at = now(), finished_at = NULL, last_error = NULL",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .bind(high.max(from))
    .bind(total)
    .bind(from)
    .execute(&mut *conn)
    .await
    .map_err(map_db)?;
    enqueue(conn, "backfill", tenant_id, table_id).await
}

async fn enqueue(
    conn: &mut PgConnection,
    op: &str,
    tenant_id: &str,
    table_id: Uuid,
) -> Result<(), DataError> {
    enqueue_op(conn, op, tenant_id, table_id).await
}

/// 索引系ジョブ（`backfill` / `composite` / `parity` / `derived` / `purge`）を同じ Tx で積む。
///
/// `derived`（再計算待ちをさらう）と `purge`（降格した索引行を消す）は実行時に台帳を読み直すので、
/// 同じテーブルの未着手のジョブがあれば積まない（参照先の更新ごとに同じジョブが溜まるのを防ぐ・
/// Task 15.6。着手済みのジョブは数えない＝走査後の書込を取りこぼさない・[`jobq::enqueue_unless_pending_on`]）。
pub(crate) async fn enqueue_op(
    conn: &mut PgConnection,
    op: &str,
    tenant_id: &str,
    table_id: Uuid,
) -> Result<(), DataError> {
    let payload = json!({ "op": op, "table_id": table_id, "v": 1 });
    let job = jobq::NewJob {
        queue: INDEX_BACKFILL_QUEUE,
        tenant_id,
        payload: &payload,
        trace_id: None,
        max_attempts: 10,
    };
    let r = if matches!(op, "derived" | "purge") {
        jobq::enqueue_unless_pending_on(conn, job).await.map(|_| ())
    } else {
        jobq::enqueue_on(conn, job).await.map(|_| ())
    };
    r.map_err(|e| DataError::Internal(format!("jobq: {e}")))
}

/// `building` の unique 索引を**この Tx で**全レコードから作り、`active` にする（`data_table` 行を
/// `FOR UPDATE` 済みで、並行する書込は直列化されている前提）。重複があれば 409（旧方式で一意な式
/// インデックスの作成が失敗したのと同じ）。
pub(crate) async fn build_unique_now(
    conn: &mut PgConnection,
    keys: TableKeys,
    schema: &TableSchema,
    tenant_id: &str,
) -> Result<(), DataError> {
    let targets: Vec<(FieldId, IndexTable)> = load_states(conn, keys)
        .await?
        .into_iter()
        .filter(|(_, t, s)| *t == IndexTable::Unique && *s == IndexState::Building)
        .map(|(f, t, _)| (f, t))
        .collect();
    if targets.is_empty() {
        return Ok(());
    }
    let plan = IndexPlan::keys_only(keys);
    let writer = IndexWriter {
        plan: &plan,
        schema,
        tenant_id,
        max_multi_values: MAX_MULTI_VALUES_CEILING,
    };
    let mut after = 0i64;
    loop {
        let rows: Vec<SourceRow> = sqlx::query_as(
            "SELECT record_int, owner, created_at, updated_at, data FROM data_record \
             WHERE tenant_int = $1 AND table_int = $2 AND record_int > $3 \
             ORDER BY record_int LIMIT $4",
        )
        .bind(keys.tenant_int)
        .bind(keys.table_int)
        .bind(after)
        .bind(BACKFILL_CHUNK)
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db)?;
        let Some(last) = rows.last().map(|r| r.record_int) else {
            break;
        };
        for r in &rows {
            let data = decode_record(schema, &r.data.0);
            writer
                .rebuild(
                    conn,
                    &IndexedRecord {
                        record_int: r.record_int,
                        owner: &r.owner,
                        created_at: r.created_at,
                        updated_at: r.updated_at,
                        data: &data,
                    },
                    &targets,
                )
                .await
                .map_err(|e| match e {
                    DataError::Conflict(_) => {
                        DataError::Conflict("重複値があるため unique を適用できません".into())
                    }
                    other => other,
                })?;
        }
        after = last;
    }
    for (field, table) in &targets {
        super::state::set_state(conn, keys, *field, *table, IndexState::Active).await?;
    }
    Ok(())
}

/// 台帳初期化で読む data_table の列（schema, key_format, tenant_int, table_int, pivot_index_init）。
type InitRow = (Json<TableSchema>, i16, i32, i64, bool);

/// バックフィル対象レコードの行。
#[derive(sqlx::FromRow)]
struct SourceRow {
    record_int: i64,
    owner: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    data: Json<Value>,
}

#[derive(sqlx::FromRow)]
struct ProgressRow {
    cursor_record: i64,
    high_water: i64,
    status: String,
}

impl DataStore {
    /// 台帳が未初期化のテーブルを初期化する（冪等）。初期化した `true`／済み `false`。
    ///
    /// `data_table` 行を `FOR UPDATE` で押さえ、台帳を作り、unique を同期構築し、既存レコードがあれば
    /// バックフィルを積む。v1 形式（識別子未移行）のテーブルは先に識別子を移行すること。
    pub async fn init_pivot_index(
        &self,
        tenant_id: &str,
        table_id: Uuid,
    ) -> Result<bool, DataError> {
        let mut tx = self
            .pools
            .for_tenant(tenant_id)
            .begin()
            .await
            .map_err(map_db)?;
        let row: Option<InitRow> = sqlx::query_as(
            "SELECT schema, key_format, tenant_int, table_int, pivot_index_init FROM data_table \
             WHERE tenant_id = $1 AND id = $2 FOR UPDATE",
        )
        .bind(tenant_id)
        .bind(table_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db)?;
        let (schema, key_format, tenant_int, table_int, init) = row.ok_or(DataError::NotFound)?;
        if init {
            tx.rollback().await.map_err(map_db)?;
            return Ok(false);
        }
        if key_format == KEY_FORMAT_LEGACY {
            return Err(DataError::Conflict(
                "識別子の移行が完了していないテーブルの索引は初期化できません".into(),
            ));
        }
        let keys = TableKeys {
            tenant_int,
            table_int,
        };
        let has_rows = table_has_rows(&mut tx, keys).await?;
        if sync_ledger(&mut tx, keys, &schema.0, has_rows).await? {
            // unique は同じ Tx で同期構築して active にする（一意性の強制に空白期間を作らない・
            // 旧方式の一意な式インデックスの代わり）。残りはバックフィル。
            build_unique_now(&mut tx, keys, &schema.0, tenant_id).await?;
            start_backfill(&mut tx, keys, tenant_id, table_id).await?;
        }
        sqlx::query(
            "UPDATE data_table SET pivot_index_init = true WHERE tenant_id = $1 AND id = $2",
        )
        .bind(tenant_id)
        .bind(table_id)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        tx.commit().await.map_err(map_db)?;
        tracing::info!(%table_id, has_rows, "索引台帳を初期化しました");
        Ok(true)
    }

    /// バックフィルを 1 チャンク（最大 `max_records` 件）進める。冪等・中断再開可。
    pub async fn index_backfill_step(
        &self,
        tenant_id: &str,
        table_id: Uuid,
        max_records: i64,
    ) -> Result<IndexBackfillStep, DataError> {
        let result = self.backfill_chunk(tenant_id, table_id, max_records).await;
        if let Err(e) = &result {
            // 失敗は進捗に記録する（チャンクはロールバック済み・次回は同じ位置から）。
            if let Err(re) = self.record_backfill_error(tenant_id, table_id, e).await {
                tracing::error!(error = %re, "バックフィル失敗の記録に失敗");
            }
        }
        result
    }

    async fn backfill_chunk(
        &self,
        tenant_id: &str,
        table_id: Uuid,
        max_records: i64,
    ) -> Result<IndexBackfillStep, DataError> {
        let mut tx = self
            .pools
            .for_tenant(tenant_id)
            .begin()
            .await
            .map_err(map_db)?;
        let meta: Option<(i32, i64)> = sqlx::query_as(
            "SELECT tenant_int, table_int FROM data_table WHERE tenant_id = $1 AND id = $2",
        )
        .bind(tenant_id)
        .bind(table_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db)?;
        let (tenant_int, table_int) = meta.ok_or(DataError::NotFound)?;
        let keys = TableKeys {
            tenant_int,
            table_int,
        };
        // 進捗行を先に押さえる。スキーマ改訂（索引の追加）は同じ行を書き換えてからコミットするので、
        // この後に読むスキーマと台帳は必ず同じ改訂のもの（古いスキーマで新しい索引を作り直さない）。
        let progress: Option<ProgressRow> = sqlx::query_as(
            "SELECT cursor_record, high_water, status FROM data_index_backfill \
             WHERE tenant_int = $1 AND table_int = $2 FOR UPDATE",
        )
        .bind(tenant_int)
        .bind(table_int)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db)?;
        let Some(progress) = progress.filter(|p| p.status != "done") else {
            tx.rollback().await.map_err(map_db)?;
            return Ok(IndexBackfillStep {
                processed: 0,
                done: true,
            });
        };
        let schema: Json<TableSchema> =
            sqlx::query_scalar("SELECT schema FROM data_table WHERE tenant_id = $1 AND id = $2")
                .bind(tenant_id)
                .bind(table_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(map_db)?;
        let targets: Vec<(FieldId, IndexTable)> = load_states(&mut tx, keys)
            .await?
            .into_iter()
            .filter(|(_, _, s)| *s == IndexState::Building)
            .map(|(f, t, _)| (f, t))
            .collect();
        let rows: Vec<SourceRow> = if targets.is_empty() {
            Vec::new()
        } else {
            // FOR SHARE: 並行する書込（FOR UPDATE）と直列化し、古い本体から索引を作らない。
            sqlx::query_as(
                "SELECT record_int, owner, created_at, updated_at, data FROM data_record \
                 WHERE tenant_int = $1 AND table_int = $2 AND record_int > $3 AND record_int <= $4 \
                 ORDER BY record_int LIMIT $5 FOR SHARE",
            )
            .bind(tenant_int)
            .bind(table_int)
            .bind(progress.cursor_record)
            .bind(progress.high_water)
            .bind(max_records.max(1))
            .fetch_all(&mut *tx)
            .await
            .map_err(map_db)?
        };
        let plan = IndexPlan::keys_only(keys);
        let writer = IndexWriter {
            plan: &plan,
            schema: &schema.0,
            tenant_id,
            // 過去の値は当時の上限で書かれている。上限を下げてもバックフィルは止めない。
            max_multi_values: MAX_MULTI_VALUES_CEILING,
        };
        // チャンクの全レコードを表ごとに 1 文で作り直す（1 件ずつの文にしない・§16.7）。
        let decoded: Vec<_> = rows
            .iter()
            .map(|r| decode_record(&schema.0, &r.data.0))
            .collect();
        let recs: Vec<IndexedRecord<'_>> = rows
            .iter()
            .zip(&decoded)
            .map(|(r, data)| IndexedRecord {
                record_int: r.record_int,
                owner: &r.owner,
                created_at: r.created_at,
                updated_at: r.updated_at,
                data,
            })
            .collect();
        writer.rebuild_many(&mut tx, &recs, &targets).await?;
        let processed = rows.len() as u64;
        let limit = u64::try_from(max_records.max(1)).unwrap_or(u64::MAX);
        let done = targets.is_empty() || processed < limit;
        // 一括取込が値索引を保持している間は active へ切り替えない（取込の行の索引行はまだ無い・
        // crate::bulk）。走査位置だけ末尾へ進めて running のまま置き、取込の仕上げが開始位置を決めて
        // 再開する。取込はこの進捗行を FOR UPDATE で押さえてから保持を始めるので、ここと直列化される。
        if done && crate::bulk::index_held(&mut tx, keys).await? {
            park_held(&mut tx, keys, processed).await?;
            tx.commit().await.map_err(map_db)?;
            return Ok(IndexBackfillStep { processed, done });
        }
        if done {
            activate_building(&mut tx, keys).await?;
            sqlx::query(
                "UPDATE data_index_backfill SET cursor_record = high_water, \
                   rows_done = rows_done + $3, status = 'done', finished_at = now(), \
                   updated_at = now(), last_error = NULL \
                 WHERE tenant_int = $1 AND table_int = $2",
            )
            .bind(tenant_int)
            .bind(table_int)
            .bind(i64::try_from(processed).unwrap_or(i64::MAX))
            .execute(&mut *tx)
            .await
            .map_err(map_db)?;
            // 完了と同じ Tx で parity 検証を積む（取りこぼさない）。
            enqueue(&mut tx, "parity", tenant_id, table_id).await?;
        } else {
            let last = rows.last().map_or(progress.cursor_record, |r| r.record_int);
            sqlx::query(
                "UPDATE data_index_backfill SET cursor_record = $3, rows_done = rows_done + $4, \
                   updated_at = now() \
                 WHERE tenant_int = $1 AND table_int = $2",
            )
            .bind(tenant_int)
            .bind(table_int)
            .bind(last)
            .bind(i64::try_from(processed).unwrap_or(i64::MAX))
            .execute(&mut *tx)
            .await
            .map_err(map_db)?;
        }
        tx.commit().await.map_err(map_db)?;
        Ok(IndexBackfillStep { processed, done })
    }

    async fn record_backfill_error(
        &self,
        tenant_id: &str,
        table_id: Uuid,
        e: &DataError,
    ) -> Result<(), DataError> {
        let Some(keys) = self.table_keys_of(tenant_id, table_id).await? else {
            return Ok(());
        };
        sqlx::query(
            "UPDATE data_index_backfill SET status = 'failed', last_error = $3, updated_at = now() \
             WHERE tenant_int = $1 AND table_int = $2 AND status <> 'done'",
        )
        .bind(keys.tenant_int)
        .bind(keys.table_int)
        .bind(e.to_string())
        .execute(self.pools.for_tenant(tenant_id))
        .await
        .map_err(map_db)?;
        Ok(())
    }

    /// テーブルのバックフィルを最後まで進める（同期実行・小さいテーブル向け／テスト用）。
    pub async fn backfill_table_index(
        &self,
        tenant_id: &str,
        table_id: Uuid,
    ) -> Result<(), DataError> {
        loop {
            if self
                .index_backfill_step(tenant_id, table_id, BACKFILL_CHUNK)
                .await?
                .done
            {
                return Ok(());
            }
        }
    }

    /// バックフィルの進捗（無ければ `None`＝一度もバックフィルしていない）。
    pub async fn index_backfill_status(
        &self,
        tenant_id: &str,
        table_id: Uuid,
    ) -> Result<Option<IndexBackfillStatus>, DataError> {
        let Some(keys) = self.table_keys_of(tenant_id, table_id).await? else {
            return Err(DataError::NotFound);
        };
        #[allow(clippy::type_complexity)]
        let row: Option<(
            String,
            i64,
            i64,
            i64,
            DateTime<Utc>,
            Option<DateTime<Utc>>,
            Option<DateTime<Utc>>,
            Option<i32>,
            Option<String>,
        )> = sqlx::query_as(
            "SELECT status, rows_done, rows_total, generation, started_at, finished_at, \
                    parity_checked_at, parity_mismatches, last_error \
             FROM data_index_backfill WHERE tenant_int = $1 AND table_int = $2",
        )
        .bind(keys.tenant_int)
        .bind(keys.table_int)
        .fetch_optional(self.pools.for_tenant(tenant_id))
        .await
        .map_err(map_db)?;
        Ok(row.map(
            |(status, rows_done, rows_total, generation, started_at, finished_at, pc, pm, le)| {
                IndexBackfillStatus {
                    status,
                    rows_done,
                    rows_total,
                    generation,
                    started_at,
                    finished_at,
                    parity_checked_at: pc,
                    parity_mismatches: pm,
                    last_error: le,
                }
            },
        ))
    }

    /// テーブルの内部整数キー（削除済みでも引く・保守経路用）。
    pub(crate) async fn table_keys_of(
        &self,
        tenant_id: &str,
        table_id: Uuid,
    ) -> Result<Option<TableKeys>, DataError> {
        let row: Option<(i32, i64)> = sqlx::query_as(
            "SELECT tenant_int, table_int FROM data_table WHERE tenant_id = $1 AND id = $2",
        )
        .bind(tenant_id)
        .bind(table_id)
        .fetch_optional(self.pools.for_tenant(tenant_id))
        .await
        .map_err(map_db)?;
        Ok(row.map(|(tenant_int, table_int)| TableKeys {
            tenant_int,
            table_int,
        }))
    }

    /// 台帳未初期化のテーブルを初期化し（識別子未移行なら先に移行）、取りこぼしたジョブを積み直す。
    /// 初期化したテーブル数を返す。全プールを順に処理する（合算は Rust 側）。
    pub async fn enqueue_index_backfills_if_needed(&self) -> Result<u64, DataError> {
        let mut initialized = 0;
        // リンク値の保存形の移行（v1 の単一 UUID 文字列 → 配列・crate::link_values）を積む。
        if let Err(e) = self.enqueue_link_value_migrations(INIT_SWEEP_BATCH).await {
            tracing::warn!(error = %e, "リンク値の移行ジョブの投入に失敗（次のスイープで再試行）");
        }
        for (_, pool) in self.pools.all() {
            // shard-safety: allow(cross-tenant) 保守スイープ。プールごとに問い合わせる（合算は Rust 側）。
            let targets: Vec<(String, Uuid, i16)> = sqlx::query_as(
                "SELECT tenant_id, id, key_format FROM data_table \
                 WHERE pivot_index_init = false AND deleted_at IS NULL \
                 ORDER BY tenant_id, id LIMIT $1",
            )
            .bind(INIT_SWEEP_BATCH)
            .fetch_all(pool)
            .await
            .map_err(map_db)?;
            for (tenant_id, id, key_format) in targets {
                // 1 テーブルの失敗（移行できない古いスキーマ等）でスイープ全体を止めない。
                let r = async {
                    if key_format == KEY_FORMAT_LEGACY {
                        self.migrate_table_field_keys(&tenant_id, id).await?;
                    }
                    self.init_pivot_index(&tenant_id, id).await
                }
                .await;
                match r {
                    Ok(true) => initialized += 1,
                    Ok(false) => {}
                    Err(e) => {
                        tracing::warn!(error = %e, table_id = %id, "索引台帳の初期化に失敗（次のスイープで再試行）");
                    }
                }
            }
            // ジョブを失った running のバックフィル（投入 Tx の後にキューが失われた等）を積み直す。
            let mut tx = pool.begin().await.map_err(map_db)?;
            if !jobq::has_pending(&mut tx, INDEX_BACKFILL_QUEUE)
                .await
                .map_err(|e| DataError::Internal(format!("jobq: {e}")))?
            {
                // shard-safety: allow(cross-tenant) 保守スイープ（進捗表から未完了のテーブルを拾う）。
                let running: Vec<i64> = sqlx::query_scalar(
                    "SELECT table_int FROM data_index_backfill WHERE status <> 'done' LIMIT 100",
                )
                .fetch_all(&mut *tx)
                .await
                .map_err(map_db)?;
                // shard-safety: allow(cross-tenant) 保守スイープ（未完了のリンク値の移行も同様に拾う）。
                let links: Vec<i64> = sqlx::query_scalar(
                    "SELECT table_int FROM data_link_value_migration WHERE status <> 'done' LIMIT 100",
                )
                .fetch_all(&mut *tx)
                .await
                .map_err(map_db)?;
                let ops = running
                    .into_iter()
                    .map(|t| (t, "backfill"))
                    .chain(links.into_iter().map(|t| (t, LINK_VALUES_OP)));
                for (table_int, op) in ops {
                    // shard-safety: allow(cross-tenant) table_int はグローバル一意。所属テナントを引く。
                    let owner: Option<(String, Uuid)> =
                        sqlx::query_as("SELECT tenant_id, id FROM data_table WHERE table_int = $1")
                            .bind(table_int)
                            .fetch_optional(&mut *tx)
                            .await
                            .map_err(map_db)?;
                    if let Some((tenant_id, id)) = owner {
                        enqueue(&mut tx, op, &tenant_id, id).await?;
                    }
                }
            }
            tx.commit().await.map_err(map_db)?;
        }
        Ok(initialized)
    }

    /// バックフィル／parity ジョブを 1 件消費する。処理した場合 `true`。
    pub async fn consume_index_job_once(&self) -> Result<bool, DataError> {
        for (_, pool) in self.pools.all() {
            let mut conn = pool.acquire().await.map_err(map_db)?;
            let mut jobs = jobq::claim(&mut conn, INDEX_BACKFILL_QUEUE, BACKFILL_VT, 1)
                .await
                .map_err(|e| DataError::Internal(format!("jobq: {e}")))?;
            drop(conn);
            let Some(job) = jobs.pop() else { continue };
            let outcome = self.run_index_job(&job).await;
            let mut tx = pool.begin().await.map_err(map_db)?;
            match outcome {
                Ok(continue_backfill) => {
                    if continue_backfill {
                        if let Some(table_id) = job_table(&job.payload) {
                            // 続きは同じ op で積む（composite / derived の続きを backfill にしない）。
                            let op = job
                                .payload
                                .get("op")
                                .and_then(Value::as_str)
                                .unwrap_or("backfill");
                            enqueue(&mut tx, op, &job.tenant_id, table_id).await?;
                        }
                    }
                    jobq::ack(&mut tx, job.id)
                        .await
                        .map_err(|e| DataError::Internal(format!("jobq: {e}")))?;
                    tx.commit().await.map_err(map_db)?;
                }
                Err(e) => {
                    let failed = jobq::fail(
                        &mut tx,
                        job.id,
                        &e.to_string(),
                        jobq::backoff_for(job.attempts),
                    )
                    .await;
                    if failed.is_ok() {
                        tx.commit().await.map_err(map_db)?;
                    }
                    return Err(e);
                }
            }
            return Ok(true);
        }
        Ok(false)
    }

    /// ジョブ 1 件を処理する。バックフィルが未完なら `true`（続きのジョブを積む）。
    async fn run_index_job(&self, job: &jobq::ClaimedJob) -> Result<bool, DataError> {
        let Some(table_id) = job_table(&job.payload) else {
            return Err(DataError::Internal(
                "索引ジョブの payload が不正です".into(),
            ));
        };
        match job.payload.get("op").and_then(Value::as_str) {
            Some("backfill") => {
                for _ in 0..CHUNKS_PER_JOB {
                    match self
                        .index_backfill_step(&job.tenant_id, table_id, BACKFILL_CHUNK)
                        .await
                    {
                        Ok(step) if step.done => return Ok(false),
                        Ok(_) => {}
                        Err(DataError::NotFound) => return Ok(false), // テーブルが消えた
                        Err(e) => return Err(e),
                    }
                }
                Ok(true)
            }
            Some("composite") => {
                for _ in 0..CHUNKS_PER_JOB {
                    match self
                        .sort_set_backfill_step(&job.tenant_id, table_id, BACKFILL_CHUNK)
                        .await
                    {
                        Ok(true) | Err(DataError::NotFound) => return Ok(false), // 完了・テーブルが消えた
                        Ok(false) => {}
                        Err(e) => return Err(e),
                    }
                }
                Ok(true)
            }
            Some("derived") => {
                // 材料化した派生列の再計算待ち（Task 15.4・derived::materialize）。
                for _ in 0..CHUNKS_PER_JOB {
                    match self
                        .derived_recompute_step(
                            &job.tenant_id,
                            table_id,
                            crate::derived::materialize::RECOMPUTE_CHUNK,
                        )
                        .await
                    {
                        Ok(true) | Err(DataError::NotFound) => return Ok(false),
                        Ok(false) => {}
                        Err(e) => return Err(e),
                    }
                }
                Ok(true)
            }
            Some(LINK_VALUES_OP) => {
                // v1 の record_ref の保存値を配列へ（crate::link_values）。
                for _ in 0..CHUNKS_PER_JOB {
                    match self
                        .link_value_migration_step(&job.tenant_id, table_id, LINK_VALUE_CHUNK)
                        .await
                    {
                        Ok(step) if step.done => return Ok(false),
                        Ok(_) => {}
                        Err(DataError::NotFound) => return Ok(false), // テーブルが消えた
                        Err(e) => return Err(e),
                    }
                }
                Ok(true)
            }
            Some("purge") => {
                // 大きなテーブルの索引の降格の後片付け（crate::index::promote）。
                for _ in 0..CHUNKS_PER_JOB {
                    if self.index_purge_step(&job.tenant_id, table_id).await? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            Some("parity") => {
                let report = match self.verify_index_parity(&job.tenant_id, table_id).await {
                    Ok(r) => r,
                    // 完了直後にテーブルが削除・テナントが purge された（検証する対象が無い）。
                    Err(DataError::NotFound) => return Ok(false),
                    Err(e) => return Err(e),
                };
                if !report.is_consistent() {
                    tracing::error!(%table_id, mismatches = ?report.mismatches, "索引の parity 検証で不一致");
                }
                Ok(false)
            }
            _ => Err(DataError::Internal("索引ジョブの op が不正です".into())),
        }
    }
}

/// 一括取込が値索引を保持している間の走査の終わり: active へ切り替えず、走査位置だけ末尾へ進めて
/// running のまま置く（取込の仕上げが開始位置を決めて再開する・[`crate::bulk`]）。
async fn park_held(
    conn: &mut PgConnection,
    keys: TableKeys,
    processed: u64,
) -> Result<(), DataError> {
    sqlx::query(
        "UPDATE data_index_backfill SET cursor_record = high_water, \
           rows_done = rows_done + $3, updated_at = now() \
         WHERE tenant_int = $1 AND table_int = $2",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .bind(i64::try_from(processed).unwrap_or(i64::MAX))
    .execute(&mut *conn)
    .await
    .map_err(map_db)?;
    Ok(())
}

fn job_table(payload: &Value) -> Option<Uuid> {
    payload
        .get("table_id")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
}

pub(crate) async fn table_has_rows(
    conn: &mut PgConnection,
    keys: TableKeys,
) -> Result<bool, DataError> {
    sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM data_record WHERE tenant_int = $1 AND table_int = $2)",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db)
}

/// 索引バックフィルの常駐ワーカー（スイープ＝初期化と積み直し・ジョブ消費）。
///
/// スキーマ改訂で索引が増えるたびにジョブが積まれるので、識別子移行と違って回し続ける。
#[derive(Clone)]
pub struct IndexBackfillWorker {
    store: DataStore,
    poll: Duration,
}

impl IndexBackfillWorker {
    pub fn new(store: DataStore) -> Self {
        IndexBackfillWorker {
            store,
            poll: Duration::from_secs(15),
        }
    }

    /// バックグラウンドで回す。
    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move { self.run().await })
    }

    async fn run(self) {
        loop {
            if let Err(e) = self.store.enqueue_index_backfills_if_needed().await {
                tracing::warn!(error = %e, "索引台帳の初期化スイープに失敗");
            }
            loop {
                match self.store.consume_index_job_once().await {
                    Ok(true) => {}
                    Ok(false) => break,
                    Err(e) => {
                        tracing::warn!(error = %e, "索引バックフィルジョブに失敗（jobq がバックオフ再配信）");
                        break;
                    }
                }
            }
            tokio::time::sleep(self.poll).await;
        }
    }
}
