//! フィールド識別子の移行ジョブ（Phase 15 Task 15.1・data-platform §4.3 / §13 M1）。
//!
//! v1 のテーブル（`key_format = 1`: JSONB キー = フィールド名）を 3 層形式
//! （`key_format = 2`: JSONB キー = `"f{id}"`）へ**テーブル単位**で移行する。
//!
//! ```text
//! name = "applicant" → id = 宣言順に 16 から / key = "applicant" / display_name = "applicant"
//! JSONB: {"applicant": "佐藤"} → {"f16": "佐藤"}
//! ```
//!
//! # 冪等・中断再開
//!
//! - 1 テーブル = 1 トランザクション。`data_table` 行を `FOR UPDATE` で押さえてから
//!   `key_format` を見るので、ジョブの多重実行・アクセス時の遅延移行（[`DataStore::fetch_live`]）
//!   と並行しても 1 回しか書き換わらない。途中で落ちたテーブルはロールバックされ、旧形式のまま残る。
//! - スイープは jobq のジョブ 1 件 = 最大 [`MIGRATION_BATCH`] テーブル。残りがあれば同じ Tx で
//!   続きのジョブを積んでから ack する。ワーカーが落ちても可視タイムアウト後に再配信され、
//!   移行済みテーブルは `key_format = 2` なので飛ばされる（＝中断した所から再開する）。
//! - `id` の採番は決定的（[`upgrade_legacy_schema`]）。移行前に一覧で見せた `id` と移行後の実データが一致する。
//!
//! 書き換えはレコードの `rev` / `updated_at` を動かさない（利用者の変更ではないため）。
//! リビジョン履歴の差分は外部契約の `key` で記録しているので書き換え不要。

use std::time::Duration;

use serde_json::json;
use sqlx::types::Json;
use uuid::Uuid;

use crate::field_key::upgrade_legacy_schema;
use crate::model::{FieldType, TableSchema};
use crate::store::DataStore;
use crate::{map_db, DataError};

/// v1 形式（JSONB キー = フィールド名）。
pub(crate) const KEY_FORMAT_LEGACY: i16 = 1;
/// 3 層形式（JSONB キー = `"f{id}"`）。
pub(crate) const KEY_FORMAT_V2: i16 = 2;

/// 移行ジョブのキュー名。
pub const FIELD_KEY_MIGRATION_QUEUE: &str = "data_field_key_migration";
/// ジョブ 1 件で移行するテーブル数の上限。
pub const MIGRATION_BATCH: i64 = 50;
/// 可視タイムアウト（1 バッチの想定所要より十分長く）。
const MIGRATION_VT: Duration = Duration::from_mins(15);
/// advisory lock のキー（二重投入防止・値は任意の定数）。
const ENQUEUE_LOCK_KEY: i64 = 0x5348_494b_4931_3531; // "SHIKI151"

/// スイープ 1 回の結果（進捗の観測用）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeyMigrationProgress {
    /// このバッチで移行したテーブル数。
    pub migrated: u64,
    /// このバッチで移行に失敗したテーブル数（ロールバック済み・v1 形式のまま）。
    pub failed: u64,
    /// まだ v1 形式のテーブル数（全プール合算）。
    pub remaining: i64,
}

impl DataStore {
    /// 1 テーブルを 3 層形式へ移行する（冪等）。移行した `true`／既に移行済み `false`。
    pub async fn migrate_table_field_keys(
        &self,
        tenant_id: &str,
        table_id: Uuid,
    ) -> Result<bool, DataError> {
        let db = self.pools.for_tenant(tenant_id);
        let mut tx = db.begin().await.map_err(map_db)?;
        let row: Option<(Json<TableSchema>, i16, i32, i64)> = sqlx::query_as(
            "SELECT schema, key_format, tenant_int, table_int FROM data_table \
             WHERE tenant_id = $1 AND id = $2 FOR UPDATE",
        )
        .bind(tenant_id)
        .bind(table_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db)?;
        let (legacy, format, tenant_int, table_int) = row.ok_or(DataError::NotFound)?;
        if format != KEY_FORMAT_LEGACY {
            tx.rollback().await.map_err(map_db)?;
            return Ok(false);
        }
        let (schema, next_field_id) = upgrade_legacy_schema(&legacy.0)?;
        crate::schema::validate_table_schema(&schema)?;

        // ② JSONB キーを書き換える（実フィールドのみ。派生フィールドは保存されていない）。
        let (old_keys, new_keys): (Vec<String>, Vec<String>) = schema
            .fields
            .iter()
            .filter(|f| !matches!(f.field_type, FieldType::Lookup | FieldType::Computed))
            .map(|f| (f.key.clone(), f.json_key()))
            .unzip();
        let rewritten = sqlx::query(
            "UPDATE data_record SET data = ( \
                 SELECT coalesce(jsonb_object_agg(m.new_key, d.value), '{}'::jsonb) \
                 FROM jsonb_each(data_record.data) d \
                 JOIN unnest($3::text[], $4::text[]) AS m(old_key, new_key) \
                   ON m.old_key = d.key) \
             WHERE tenant_int = $5 AND table_int = $6 AND tenant_id = $1 AND table_id = $2",
        )
        .bind(tenant_id)
        .bind(table_id)
        .bind(&old_keys)
        .bind(&new_keys)
        .bind(tenant_int)
        .bind(table_int)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?
        .rows_affected();
        // ③ 索引は pivot 索引テーブル（台帳の初期化とバックフィル・unique は同期構築）で張る
        //    （旧方式の式インデックスは Task 15.3 で撤去済み）。
        // ④ スキーマと形式を確定する（schema_version は上げない＝意味は変わらない）。
        sqlx::query(
            "UPDATE data_table SET schema = $3, key_format = $4, next_field_id = $5 \
             WHERE tenant_id = $1 AND id = $2",
        )
        .bind(tenant_id)
        .bind(table_id)
        .bind(Json(&schema))
        .bind(KEY_FORMAT_V2)
        .bind(next_field_id)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        tx.commit().await.map_err(map_db)?;
        tracing::info!(%table_id, records = rewritten, "フィールド識別子を 3 層形式へ移行しました");
        Ok(true)
    }

    /// 未移行テーブルを最大 `batch` 件移行する（全プールを順に処理し、残数を合算する）。
    ///
    /// 1 テーブルの失敗はそのテーブルだけロールバックして次へ進む（`failed` に数える）。
    /// 1 件も進まずに失敗だけがあった場合はエラーを返し、jobq のバックオフ・DLQ に委ねる
    /// （直らないテーブルで空回りしない・運用者に見える）。
    pub async fn migrate_field_keys_batch(
        &self,
        batch: i64,
    ) -> Result<KeyMigrationProgress, DataError> {
        let mut progress = KeyMigrationProgress::default();
        let mut last_error: Option<DataError> = None;
        let mut budget = batch.max(1);
        for (_, pool) in self.pools.all() {
            if budget <= 0 {
                break;
            }
            // shard-safety: allow(cross-tenant) 保守スイープ。プールごとに問い合わせる（合算は Rust 側）。
            let targets: Vec<(String, Uuid)> = sqlx::query_as(
                "SELECT tenant_id, id FROM data_table WHERE key_format = 1 \
                 ORDER BY tenant_id, id LIMIT $1",
            )
            .bind(budget)
            .fetch_all(pool)
            .await
            .map_err(map_db)?;
            for (tenant_id, id) in targets {
                budget -= 1;
                match self.migrate_table_field_keys(&tenant_id, id).await {
                    Ok(true) => progress.migrated += 1,
                    Ok(false) => {}
                    Err(e) => {
                        tracing::error!(table_id = %id, error = %e, "フィールド識別子の移行に失敗（このテーブルは v1 形式のまま）");
                        progress.failed += 1;
                        last_error = Some(e);
                    }
                }
            }
        }
        progress.remaining = self.migrate_field_keys_remaining().await?;
        if let Some(e) = last_error {
            if progress.migrated == 0 {
                return Err(e);
            }
        }
        Ok(progress)
    }

    /// 未移行テーブルがあり、移行ジョブが積まれていなければ 1 件積む。積んだら `true`。
    ///
    /// 判定と投入は advisory lock 下の同一 Tx（全レプリカが起動時に呼んでも二重投入しない）。
    pub async fn enqueue_field_key_migration_if_needed(&self) -> Result<bool, DataError> {
        let mut legacy = false;
        for (_, pool) in self.pools.all() {
            // shard-safety: allow(cross-tenant) 保守ジョブの要否判定（プールごと）。
            let exists: bool =
                sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM data_table WHERE key_format = 1)")
                    .fetch_one(pool)
                    .await
                    .map_err(map_db)?;
            legacy |= exists;
        }
        if !legacy {
            return Ok(false);
        }
        let mut tx = self.pools.registry().begin().await.map_err(map_db)?;
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(ENQUEUE_LOCK_KEY)
            .execute(&mut *tx)
            .await
            .map_err(map_db)?;
        if jobq::has_pending(&mut tx, FIELD_KEY_MIGRATION_QUEUE)
            .await
            .map_err(map_jobq)?
        {
            tx.commit().await.map_err(map_db)?;
            return Ok(false);
        }
        enqueue(&mut tx).await?;
        tx.commit().await.map_err(map_db)?;
        Ok(true)
    }

    /// 移行ジョブを 1 件消費する。処理した場合 `Some(進捗)`。
    pub async fn consume_field_key_migration_once(
        &self,
    ) -> Result<Option<KeyMigrationProgress>, DataError> {
        let registry = self.pools.registry();
        let mut conn = registry.acquire().await.map_err(map_db)?;
        let mut jobs = jobq::claim(&mut conn, FIELD_KEY_MIGRATION_QUEUE, MIGRATION_VT, 1)
            .await
            .map_err(map_jobq)?;
        let Some(job) = jobs.pop() else {
            return Ok(None);
        };
        // 移行中にコネクションを握らない（移行は別コネクションで Tx を張る）。
        drop(conn);
        let outcome = self.migrate_field_keys_batch(MIGRATION_BATCH).await;
        let mut tx = registry.begin().await.map_err(map_db)?;
        match outcome {
            Ok(progress) => {
                // 続きがあれば**同じ Tx で**次のジョブを積んでから ack する（取りこぼさない）。
                if progress.remaining > 0 {
                    enqueue(&mut tx).await?;
                }
                jobq::ack(&mut tx, job.id).await.map_err(map_jobq)?;
                tx.commit().await.map_err(map_db)?;
                Ok(Some(progress))
            }
            Err(e) => {
                let failed = jobq::fail(
                    &mut tx,
                    job.id,
                    &e.to_string(),
                    jobq::backoff_for(job.attempts),
                )
                .await;
                if let Err(fe) = failed {
                    tracing::error!(error = %fe, "移行ジョブの失敗記録に失敗（可視タイムアウト後に再配信）");
                } else if let Err(ce) = tx.commit().await {
                    tracing::error!(error = %ce, "移行ジョブの失敗記録のコミットに失敗");
                }
                Err(e)
            }
        }
    }
}

async fn enqueue(conn: &mut sqlx::PgConnection) -> Result<(), DataError> {
    // tenant_id は jobq の必須列だが、このジョブは全テナント横断の保守ジョブ（固定値）。
    jobq::enqueue_on(
        conn,
        jobq::NewJob {
            queue: FIELD_KEY_MIGRATION_QUEUE,
            tenant_id: "-",
            payload: &json!({ "job": "field_key_migration", "v": 1 }),
            trace_id: None,
            max_attempts: 10,
        },
    )
    .await
    .map_err(map_jobq)?;
    Ok(())
}

#[allow(clippy::needless_pass_by_value)]
fn map_jobq(e: jobq::JobqError) -> DataError {
    DataError::Internal(format!("jobq: {e}"))
}

/// 移行ジョブの常駐ワーカー（投入と消費の両方を持つ）。
///
/// 起動時に要否を判定して投入し、ジョブを消費し尽くしたら終了する（移行は一方向で、新規
/// テーブルは最初から 3 層形式のため、完了後に回し続ける意味が無い）。
#[derive(Clone)]
pub struct FieldKeyMigrationWorker {
    store: DataStore,
    poll: Duration,
}

impl FieldKeyMigrationWorker {
    pub fn new(store: DataStore) -> Self {
        FieldKeyMigrationWorker {
            store,
            poll: Duration::from_secs(30),
        }
    }

    /// バックグラウンドで回す。
    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move { self.run().await })
    }

    async fn run(self) {
        loop {
            match self.store.enqueue_field_key_migration_if_needed().await {
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "識別子移行ジョブの投入判定に失敗"),
            }
            match self.store.consume_field_key_migration_once().await {
                Ok(Some(p)) => {
                    tracing::info!(
                        migrated = p.migrated,
                        failed = p.failed,
                        remaining = p.remaining,
                        "フィールド識別子の移行バッチを処理しました"
                    );
                    if p.remaining == 0 {
                        tracing::info!("フィールド識別子の移行が完了しました");
                        return;
                    }
                }
                Ok(None) => {
                    // 積まれていない＝未移行テーブルが無い（または他レプリカが処理中）。
                    match self.store.migrate_field_keys_remaining().await {
                        Ok(0) => return,
                        Ok(_) => {}
                        Err(e) => tracing::warn!(error = %e, "移行残数の取得に失敗"),
                    }
                    tokio::time::sleep(self.poll).await;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "識別子移行バッチに失敗（jobq がバックオフ再配信）");
                    tokio::time::sleep(self.poll).await;
                }
            }
        }
    }
}

impl DataStore {
    /// 未移行テーブル数（全プール合算・進捗の観測用）。
    pub async fn migrate_field_keys_remaining(&self) -> Result<i64, DataError> {
        let mut total = 0;
        for (_, pool) in self.pools.all() {
            // shard-safety: allow(cross-tenant) 進捗の観測（プールごとに数えて合算する）。
            let n: i64 = sqlx::query_scalar("SELECT count(*) FROM data_table WHERE key_format = 1")
                .fetch_one(pool)
                .await
                .map_err(map_db)?;
            total += n;
        }
        Ok(total)
    }
}
