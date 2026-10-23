//! 統計収集ジョブ（query-optimizer.md §4.2・jobq [`STATS_ANALYZE_QUEUE`]）。
//!
//! 1 テーブルについて次を作り直す。起動は「前回からの変更行数 > 50 + 0.1 × 行数」（[`super::counters`]）・
//! 索引の昇格完了時（[`crate::index`]）・推定外れの継続時（フィードバック・[`crate::query::planner::feedback`]）。
//!
//! 1. **行数と値頻度表の数え直し**（台帳 consumer のロックの下・1 文 = 1 スナップショット）。同じ文で
//!    「まだ消費していない変更」を拾って `skip_events` に置く（数え直しに含まれているので消費側は足さない）。
//! 2. **フィールド統計**: 値索引 `(tenant_int, table_int, field_id, val, record_int)` を値順に読み、
//!    値を持つ行数・索引行数・異なり数・最頻値 100 件・等深ヒストグラム 100 区間・切り詰め率・
//!    多値の平均要素数・値の並びと作成順（`record_int`）の相関を作る。索引行が
//!    [`ANALYZE_FULL_SCAN_MAX`] を超えたら `record_int` で間引く（行数に比例）。材料化した派生列・擬似列も含む。
//! 3. **リンク統計**（リンク列・対称列の方向ごと）と**行幅**（本体の平均バイト数・TOAST 率）。
//!
//! **公平性**: ジョブはテナントごとの先頭だけを取り出し（[`jobq::claim_fair`]）、各文の間に処理時間と
//! 同じだけ休む。文の実行中はそのテナントの読取の同時実行枠（data-platform §16.8・
//! [`crate::query::admission`]）を消費するので、重いテナントの統計収集はそのテナントの枠の中で待つ。

use std::time::{Duration, Instant};

use serde_json::Value;
use sqlx::types::Json;
use sqlx::{PgConnection, Row};
use uuid::Uuid;

use super::{
    counts_eligible, ANALYZE_FULL_SCAN_MAX, STATS_ANALYZE_QUEUE, STATS_CONSUMER,
    VALUE_COUNT_MAX_DISTINCT,
};
use crate::index::state::load_states;
use crate::index::{IndexState, IndexTable};
use crate::key_migration::KEY_FORMAT_LEGACY;
use crate::model::{FieldId, TableKeys, TableSchema, SYSTEM_FIELD_CREATED_AT, SYSTEM_FIELD_OWNER};
use crate::store::DataStore;
use crate::stream::outbox::RECORD_CHANGED_EVENT;
use crate::{map_db, DataError};

/// ジョブの可視性タイムアウト（大きなテーブルの収集が終わる前に再配信しない）。
const ANALYZE_VT: Duration = Duration::from_mins(15);
/// 統計収集の文の `statement_timeout`（ミリ秒）。
const ANALYZE_STATEMENT_TIMEOUT_MS: u64 = 120_000;
/// 等深ヒストグラムの区間数。
const HISTOGRAM_BUCKETS: usize = 100;

/// 1 回の統計収集の結果（運用・テスト用。統計値そのものは含まない）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AnalyzeOutcome {
    /// 作り直したフィールド統計の数。
    pub fields: usize,
    /// 作り直したリンク統計の数。
    pub links: usize,
    /// 値頻度表を持つ列の数（owner 擬似列を含む）。
    pub counted_fields: usize,
    /// 間引いて作った列があるか。
    pub sampled: bool,
}

impl DataStore {
    /// 統計収集ジョブを 1 件処理する（テナントごとの先頭から公平に取り出す）。処理したら `true`。
    pub async fn run_stats_job_once(&self) -> Result<bool, DataError> {
        for (_, pool) in self.pools.all() {
            let mut conn = pool.acquire().await.map_err(map_db)?;
            let mut jobs = jobq::claim_fair(&mut conn, STATS_ANALYZE_QUEUE, ANALYZE_VT, 1)
                .await
                .map_err(|e| DataError::Internal(format!("jobq: {e}")))?;
            drop(conn);
            let Some(job) = jobs.pop() else { continue };
            let table_id = job
                .payload
                .get("table_id")
                .and_then(Value::as_str)
                .and_then(|s| Uuid::parse_str(s).ok());
            let outcome = match table_id {
                Some(t) => self.analyze_table(&job.tenant_id, t).await.map(|_| ()),
                None => Err(DataError::Internal(
                    "統計収集ジョブの payload が不正です".into(),
                )),
            };
            let mut tx = pool.begin().await.map_err(map_db)?;
            match outcome {
                Ok(()) => {
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

    /// 1 テーブルの統計を作り直す（保守経路・主体を持たない）。テーブルが無ければ何もしない。
    pub async fn analyze_table(
        &self,
        tenant_id: &str,
        table_id: Uuid,
    ) -> Result<AnalyzeOutcome, DataError> {
        let pool = self.pools.for_tenant(tenant_id);
        let row: Option<(i32, i64, Json<TableSchema>, i16)> = sqlx::query_as(
            "SELECT tenant_int, table_int, schema, key_format FROM data_table \
             WHERE tenant_id = $1 AND id = $2 AND deleted_at IS NULL",
        )
        .bind(tenant_id)
        .bind(table_id)
        .fetch_optional(pool)
        .await
        .map_err(map_db)?;
        let Some((tenant_int, table_int, Json(schema), key_format)) = row else {
            return Ok(AnalyzeOutcome::default());
        };
        if key_format == KEY_FORMAT_LEGACY {
            // 識別子の移行が先（移行後の書込で再び起動される）。
            return Ok(AnalyzeOutcome::default());
        }
        let keys = TableKeys {
            tenant_int,
            table_int,
        };
        let mut out = AnalyzeOutcome::default();
        let mut conn = pool.acquire().await.map_err(map_db)?;
        let states = load_states(&mut conn, keys).await?;
        drop(conn);
        let active = |f: FieldId, t: IndexTable| {
            states
                .iter()
                .any(|(sf, st, s)| *sf == f && *st == t && *s == IndexState::Active)
        };
        // ① 行数と値頻度表の数え直し。
        let mut eligible: Vec<i16> = schema
            .fields
            .iter()
            .filter(|f| counts_eligible(f) && active(f.id, IndexTable::Text))
            .filter_map(|f| i16::try_from(f.id).ok())
            .collect();
        if active(SYSTEM_FIELD_OWNER, IndexTable::Text) {
            eligible.push(0);
        }
        let n_rows = if active(SYSTEM_FIELD_CREATED_AT, IndexTable::Text) {
            let started = Instant::now();
            let _permit = self.read_gate.background(tenant_id).await?;
            let (n, counted) = self.recount(tenant_id, table_id, keys, &eligible).await?;
            out.counted_fields = counted;
            pause(started).await;
            n
        } else {
            0
        };
        // ② フィールド統計（値索引のある列・擬似列・材料化した派生列）。
        let mut done: Vec<(i16, i16)> = Vec::new();
        for (field, table, state) in &states {
            if *state != IndexState::Active || !matches!(table, IndexTable::Text | IndexTable::Num)
            {
                continue;
            }
            let started = Instant::now();
            let _permit = self.read_gate.background(tenant_id).await?;
            let sampled = self
                .field_stats(tenant_id, keys, *field, *table, n_rows)
                .await?;
            out.sampled |= sampled;
            out.fields += 1;
            if let Ok(f) = i16::try_from(*field) {
                done.push((f, table.code()));
            }
            pause(started).await;
        }
        // ③ リンク統計（順方向のリンク列と対称列）と行幅。
        let started = Instant::now();
        let _permit = self.read_gate.background(tenant_id).await?;
        out.links = self
            .link_and_width_stats(tenant_id, keys, &schema, n_rows, &done)
            .await?;
        // このプロセスの計画が古い統計を使い続けないように落とす（他レプリカは寿命で入れ替わる）。
        self.stats_cache.invalidate((tenant_int, table_int));
        pause(started).await;
        Ok(out)
    }

    /// 統計収集の仕上げ: 消えた列の統計を落とし、リンク統計（方向ごと）と行幅を作り直す。
    async fn link_and_width_stats(
        &self,
        tenant_id: &str,
        keys: TableKeys,
        schema: &TableSchema,
        n_rows: i64,
        done: &[(i16, i16)],
    ) -> Result<usize, DataError> {
        let (tenant_int, table_int) = (keys.tenant_int, keys.table_int);
        let mut links = 0;
        let mut tx = self
            .pools
            .for_tenant(tenant_id)
            .begin()
            .await
            .map_err(map_db)?;
        set_timeout(&mut tx).await?;
        let (fs, ts): (Vec<i16>, Vec<i16>) = done.iter().copied().unzip();
        sqlx::query(
            "DELETE FROM data_field_stats WHERE tenant_int = $1 AND table_int = $2 \
               AND (field_id, index_table) NOT IN (SELECT * FROM unnest($3::int2[], $4::int2[]))",
        )
        .bind(tenant_int)
        .bind(table_int)
        .bind(&fs)
        .bind(&ts)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        sqlx::query("DELETE FROM data_link_stats WHERE tenant_int = $1 AND table_int = $2")
            .bind(tenant_int)
            .bind(table_int)
            .execute(&mut *tx)
            .await
            .map_err(map_db)?;
        for f in schema.fields.iter().filter(|f| f.link.is_some()) {
            let Ok(fid) = i16::try_from(f.id) else {
                continue;
            };
            let (n_src, n_edges, max_fanout): (i64, i64, i64) = sqlx::query_as(
                "SELECT count(*), coalesce(sum(s.c), 0)::int8, coalesce(max(s.c), 0)::int8 FROM ( \
                   SELECT l.src_record, count(*) AS c FROM data_link l \
                   WHERE l.tenant_int = $1 AND l.table_int = $2 AND l.field_id = $3 \
                   GROUP BY l.src_record) s",
            )
            .bind(tenant_int)
            .bind(table_int)
            .bind(fid)
            .fetch_one(&mut *tx)
            .await
            .map_err(map_db)?;
            #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
            let frac = if n_rows > 0 {
                (n_src as f64 / n_rows as f64).min(1.0) as f32
            } else {
                0.0
            };
            sqlx::query(
                "INSERT INTO data_link_stats (tenant_int, table_int, field_id, is_reverse, n_src, \
                   n_edges, max_fanout, link_frac) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            )
            .bind(tenant_int)
            .bind(table_int)
            .bind(fid)
            .bind(f.is_reverse_link())
            .bind(n_src)
            .bind(n_edges)
            .bind(i32::try_from(max_fanout).unwrap_or(i32::MAX))
            .bind(frac)
            .execute(&mut *tx)
            .await
            .map_err(map_db)?;
            links += 1;
        }
        let (width, toast): (f64, f64) = sqlx::query_as(
            "SELECT coalesce(avg(pg_column_size(r.data)), 0)::float8, \
                    coalesce(avg((pg_column_size(r.data) > 2000)::int), 0)::float8 \
             FROM (SELECT d.data FROM data_record d WHERE d.tenant_int = $1 AND d.table_int = $2 \
                   LIMIT 10000) r",
        )
        .bind(tenant_int)
        .bind(table_int)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db)?;
        #[allow(clippy::cast_possible_truncation)]
        let (width, toast) = (width as f32, toast as f32);
        sqlx::query(
            "UPDATE data_table_stats SET avg_width = $3, toast_frac = $4, analyzed_at = now(), \
               updated_at = now() WHERE tenant_int = $1 AND table_int = $2",
        )
        .bind(tenant_int)
        .bind(table_int)
        .bind(width)
        .bind(toast)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        tx.commit().await.map_err(map_db)?;
        Ok(links)
    }

    /// 行数と値頻度表を索引から数え直す（台帳 consumer のロックの下・1 文で数える）。
    /// 返り値: (行数, 値頻度表を持つ列の数)。
    async fn recount(
        &self,
        tenant_id: &str,
        table_id: Uuid,
        keys: TableKeys,
        eligible: &[i16],
    ) -> Result<(i64, usize), DataError> {
        let mut tx = self
            .pools
            .for_tenant(tenant_id)
            .begin()
            .await
            .map_err(map_db)?;
        set_timeout(&mut tx).await?;
        // 消費側と直列化する（この Tx のコミットまで消費は待つ）。
        storage::event::lock_consumer_seq(&mut tx, STATS_CONSUMER, None)
            .await
            .map_err(|e| DataError::Internal(format!("outbox: {e}")))?;
        // 1 文 = 1 スナップショット: 数え直しと「まだ消費していない変更」が同じ時点を見る。
        let rows = sqlx::query(
            "WITH skip AS ( \
               SELECT o.id FROM storage_event_outbox o \
               WHERE o.tenant_id = $3 AND o.payload ->> 'event_type' = $8 \
                 AND o.payload ->> 'table_id' = $4 \
                 AND NOT EXISTS (SELECT 1 FROM outbox_delivery d \
                                 WHERE d.consumer = $5 AND d.event_id = o.id)), \
             vc AS MATERIALIZED (SELECT x.field_id, x.val, count(DISTINCT x.record_int) AS c \
               FROM data_index_text x \
               WHERE x.tenant_int = $1 AND x.table_int = $2 AND x.field_id = ANY($6) \
               GROUP BY x.field_id, x.val), \
             over AS (SELECT v.field_id FROM vc v GROUP BY v.field_id HAVING count(*) > $7) \
             SELECT 0 AS k, vc.field_id, vc.val, vc.c FROM vc \
               WHERE vc.field_id NOT IN (SELECT field_id FROM over) \
             UNION ALL SELECT 1, NULL, NULL, (SELECT count(*) FROM data_index_text n \
               WHERE n.tenant_int = $1 AND n.table_int = $2 AND n.field_id = 1) \
             UNION ALL SELECT 2, NULL, NULL, skip.id FROM skip \
             UNION ALL SELECT 3, over.field_id, NULL, 0 FROM over",
        )
        .bind(keys.tenant_int)
        .bind(keys.table_int)
        .bind(tenant_id)
        .bind(table_id.to_string())
        .bind(STATS_CONSUMER)
        .bind(eligible)
        .bind(VALUE_COUNT_MAX_DISTINCT)
        .bind(RECORD_CHANGED_EVENT)
        .fetch_all(&mut *tx)
        .await
        .map_err(map_db)?;
        let mut n_rows = 0i64;
        let mut skip = Vec::new();
        let mut over = Vec::new();
        let (mut fields, mut vals, mut cnts) = (Vec::new(), Vec::new(), Vec::new());
        for r in rows {
            let k: i32 = r.try_get("k").map_err(map_db)?;
            let field: Option<i16> = r.try_get("field_id").map_err(map_db)?;
            let val: Option<String> = r.try_get("val").map_err(map_db)?;
            let n: i64 = r.try_get("c").map_err(map_db)?;
            match (k, field, val) {
                (0, Some(fi), Some(va)) => {
                    fields.push(fi);
                    vals.push(va);
                    cnts.push(n);
                }
                (1, _, _) => n_rows = n,
                (2, _, _) => skip.push(n),
                (3, Some(fi), _) => over.push(fi),
                _ => {}
            }
        }
        let counted: Vec<i16> = eligible
            .iter()
            .copied()
            .filter(|x| !over.contains(x))
            .collect();
        sqlx::query("DELETE FROM data_value_count WHERE tenant_int = $1 AND table_int = $2")
            .bind(keys.tenant_int)
            .bind(keys.table_int)
            .execute(&mut *tx)
            .await
            .map_err(map_db)?;
        sqlx::query(
            "INSERT INTO data_value_count (tenant_int, table_int, field_id, val, cnt) \
             SELECT $1, $2, u.f, u.v, u.c FROM unnest($3::int2[], $4::text[], $5::int8[]) AS u(f, v, c)",
        )
        .bind(keys.tenant_int)
        .bind(keys.table_int)
        .bind(&fields)
        .bind(&vals)
        .bind(&cnts)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        sqlx::query(
            "INSERT INTO data_table_stats (tenant_int, table_int, n_rows, changes_since, counts_ready, \
               counts_fields, skip_events) VALUES ($1, $2, $3, 0, true, $4, $5) \
             ON CONFLICT (tenant_int, table_int) DO UPDATE SET n_rows = excluded.n_rows, \
               changes_since = 0, counts_ready = true, counts_fields = excluded.counts_fields, \
               skip_events = excluded.skip_events, updated_at = now()",
        )
        .bind(keys.tenant_int)
        .bind(keys.table_int)
        .bind(n_rows)
        .bind(&counted)
        .bind(&skip)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        tx.commit().await.map_err(map_db)?;
        Ok((n_rows, counted.len()))
    }

    /// 1 列のフィールド統計を値索引から作る。間引いたら `true`。
    async fn field_stats(
        &self,
        tenant_id: &str,
        keys: TableKeys,
        field: FieldId,
        table: IndexTable,
        n_rows: i64,
    ) -> Result<bool, DataError> {
        let fid = i16::try_from(field)
            .map_err(|_| DataError::Internal(format!("field_id {field} が範囲外です")))?;
        // 間引きの法（索引行数の見込み ÷ 上限）。行数が分からなければ全件。
        let modulus = (n_rows / ANALYZE_FULL_SCAN_MAX + 1).max(1);
        let sampled = modulus > 1;
        #[allow(clippy::cast_precision_loss)]
        let quantiles: Vec<f64> = (0..=HISTOGRAM_BUCKETS)
            .map(|i| i as f64 / HISTOGRAM_BUCKETS as f64)
            .collect();
        let rel = table.relation();
        let text = table == IndexTable::Text;
        let truncated = if text { "x.truncated" } else { "false" };
        // 表名は固定集合（IndexTable::relation）だけ・値は全てバインド（PIT-21）。
        let sql = format!(
            "WITH s AS MATERIALIZED ( \
               SELECT x.val, x.record_int, {truncated} AS truncated FROM {rel} x \
               WHERE x.tenant_int = $1 AND x.table_int = $2 AND x.field_id = $3 \
                 AND ($4 = 1 OR x.record_int % $4 = 0)), \
             g AS MATERIALIZED (SELECT s.val, count(*) AS c FROM s GROUP BY s.val) \
             SELECT (SELECT count(*) FROM s) AS ne, \
                    (SELECT count(DISTINCT s.record_int) FROM s) AS nv, \
                    (SELECT count(*) FROM g) AS nd, \
                    (SELECT coalesce(jsonb_agg(jsonb_build_array(m.val::text, m.c) \
                              ORDER BY m.c DESC, m.val), '[]'::jsonb) \
                       FROM (SELECT g.val, g.c FROM g ORDER BY g.c DESC, g.val LIMIT 100) m) AS mcv, \
                    (SELECT coalesce(to_jsonb((percentile_disc($5::float8[]) \
                              WITHIN GROUP (ORDER BY s.val))::text[]), '[]'::jsonb) FROM s) AS hist, \
                    (SELECT coalesce(avg(s.truncated::int), 0)::float8 FROM s) AS tf, \
                    (SELECT coalesce(corr(z.rv, z.rr), 0)::float8 FROM ( \
                       SELECT dense_rank() OVER (ORDER BY s.val) AS rv, \
                              row_number() OVER (ORDER BY s.record_int) AS rr FROM s) z) AS corr"
        );
        let pool = self.pools.for_tenant(tenant_id);
        let mut tx = pool.begin().await.map_err(map_db)?;
        set_timeout(&mut tx).await?;
        let r = sqlx::query(&sql)
            .bind(keys.tenant_int)
            .bind(keys.table_int)
            .bind(fid)
            .bind(modulus)
            .bind(&quantiles)
            .fetch_one(&mut *tx)
            .await
            .map_err(map_db)?;
        let ne: i64 = r.try_get("ne").map_err(map_db)?;
        let nv: i64 = r.try_get("nv").map_err(map_db)?;
        let nd: i64 = r.try_get("nd").map_err(map_db)?;
        let mcv: Value = r.try_get("mcv").map_err(map_db)?;
        let hist: Value = r.try_get("hist").map_err(map_db)?;
        let tf: f64 = r.try_get("tf").map_err(map_db)?;
        let corr: f64 = r.try_get("corr").map_err(map_db)?;
        // 頻度は「値を持つ行」に対する割合で持つ（間引いても比は保たれる）。
        #[allow(clippy::cast_precision_loss)]
        let mcv: Vec<Value> = mcv
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|e| {
                        let v = e.get(0)?.as_str()?;
                        let c = e.get(1)?.as_i64()?;
                        Some(serde_json::json!([v, c as f64 / nv.max(1) as f64]))
                    })
                    .collect()
            })
            .unwrap_or_default();
        // 間引いたら件数を法で引き伸ばす（異なり数は、分類列＝標本の 1 割未満ならそのまま）。
        let scale = |n: i64| n.saturating_mul(modulus);
        let nd_est = if sampled && nd * 10 >= ne {
            scale(nd).min(scale(nv))
        } else {
            nd
        };
        #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
        let avg_elems = if nv > 0 {
            (ne as f64 / nv as f64) as f32
        } else {
            1.0
        };
        #[allow(clippy::cast_possible_truncation)]
        let (tf, corr) = (tf as f32, corr as f32);
        sqlx::query(
            "INSERT INTO data_field_stats (tenant_int, table_int, field_id, index_table, n_values, \
               n_entries, n_distinct, mcv, histogram, truncated_frac, avg_elems, correlation, sampled, \
               analyzed_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, now()) \
             ON CONFLICT (tenant_int, table_int, field_id, index_table) DO UPDATE SET \
               n_values = excluded.n_values, n_entries = excluded.n_entries, \
               n_distinct = excluded.n_distinct, mcv = excluded.mcv, histogram = excluded.histogram, \
               truncated_frac = excluded.truncated_frac, avg_elems = excluded.avg_elems, \
               correlation = excluded.correlation, sampled = excluded.sampled, analyzed_at = now()",
        )
        .bind(keys.tenant_int)
        .bind(keys.table_int)
        .bind(fid)
        .bind(table.code())
        .bind(scale(nv))
        .bind(scale(ne))
        .bind(nd_est)
        .bind(Json(Value::Array(mcv)))
        .bind(Json(hist))
        .bind(tf)
        .bind(avg_elems)
        .bind(corr)
        .bind(sampled)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        tx.commit().await.map_err(map_db)?;
        Ok(sampled)
    }
}

/// 統計収集を一度でも終えたか（制御列だけを読む・統計値は返さない）。
pub(crate) async fn analyzed(
    pool: &sqlx::Pool<sqlx::Postgres>,
    keys: TableKeys,
) -> Result<bool, DataError> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM data_table_stats \
           WHERE tenant_int = $1 AND table_int = $2 AND analyzed_at IS NOT NULL)",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .fetch_one(pool)
    .await
    .map_err(map_db)
}

async fn set_timeout(conn: &mut PgConnection) -> Result<(), DataError> {
    sqlx::query("SELECT set_config('statement_timeout', $1, true)")
        .bind(ANALYZE_STATEMENT_TIMEOUT_MS.to_string())
        .execute(&mut *conn)
        .await
        .map_err(map_db)?;
    Ok(())
}

/// 処理時間と同じだけ休む（統計収集の負荷を半分以下に抑える・他テナントの読取を圧迫しない）。
async fn pause(started: Instant) {
    let spent = started.elapsed().min(Duration::from_secs(5));
    if spent > Duration::from_millis(2) {
        tokio::time::sleep(spent).await;
    }
}
