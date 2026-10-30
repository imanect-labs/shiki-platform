//! ③ チャンク Tx での適用と ④ 索引の仕上げ（data-platform §16.7・§10）。
//!
//! - 1 チャンク = 1 Tx。取込行を `FOR UPDATE` で押さえ、[`lock_plan`]（テーブル行の `FOR KEY SHARE`・
//!   索引台帳）を取ってから、本体を 1 文で INSERT し、**1 件の create と同じ入口**
//!   （[`DataStore::after_records_created`]）で索引行・材料化・リビジョンを書く。同じ Tx でステージング行の
//!   状態とカーソルを進めるので、チャンク境界で冪等（落ちたチャンクは丸ごとロールバック）。
//! - チャンクの途中で unique・リンクのカーディナリティに違反した（検証の後の並行書込と競合した）ら、
//!   セーブポイントでチャンクを巻き戻し、**行ごとのセーブポイント**でやり直して違反した行だけを拒否する。
//! - 索引の遅延構築（[`hold_index`]）: 値索引を `building` に保持し、取込の行についてだけ書かない。
//!   適用が終わったら保持を外してバックフィルを開始位置つきで始め、最後まで進めてから parity を検証する。

use std::collections::{HashMap, HashSet};
use storage::audit::AuditAction;

use authz::{AuthContext, Relation};
use chrono::{DateTime, Utc};
use serde_json::{json, Map, Value};
use sqlx::types::Json;
use sqlx::{Connection, PgConnection};
use uuid::Uuid;

use super::upsert::PendingUpdate;
use super::validate::row_message;
use super::{
    index_held, load_import, ImportRow, OP_CREATE, OP_UPDATE, ST_APPLIED, ST_REJECTED, ST_STAGED,
    ST_VALID,
};
use crate::field_key::encode_record;
use crate::index::backfill::{start_backfill_from, BACKFILL_CHUNK};
use crate::index::partition::{analyze_index_partitions_without_stats, partition_holding};
use crate::index::state::{load_states, lock_plan, set_state, IndexPlan};
use crate::index::{IndexState, IndexTable};
use crate::model::{DataTable, FieldId, TableKeys, TableSchema, FIRST_USER_FIELD_ID};
use crate::record_effects::CreatedRecord;
use crate::store::DataStore;
use crate::{map_db, DataError};

/// ステージング行（row_no, 外部 ID, 状態, 正規化値, 操作, 更新の楽観ロック）。
type StagedRow = (
    i64,
    Uuid,
    i16,
    Option<Json<Map<String, Value>>>,
    i16,
    Option<i64>,
);
/// 本体の INSERT の RETURNING（record_int, 外部 ID, owner, created_at, updated_at）。
type WrittenBody = (i64, Uuid, String, DateTime<Utc>, DateTime<Utc>);
/// 適用した行（row_no, record_int, 外部 ID）。
type Applied = (i64, i64, Uuid);

/// 適用待ちの 1 行。
struct Pending {
    row_no: i64,
    id: Uuid,
    data: Map<String, Value>,
}

/// 遅延構築できる (フィールド, 索引テーブル) か。ユーザー列の値索引（text / num / search）だけ。
/// unique・リンク（強制を空白にしない）・擬似列（全件を走査できる駆動索引）・材料化列は対象外。
fn deferrable(schema: &TableSchema, field: FieldId, table: IndexTable) -> bool {
    matches!(
        table,
        IndexTable::Text | IndexTable::Num | IndexTable::Search
    ) && field >= FIRST_USER_FIELD_ID
        && schema
            .field_by_id(field)
            .is_some_and(|d| !d.field_type.is_derived())
}

/// 値索引を `building` に保持する（取込行を `FOR UPDATE` 済みの Tx 上・保持できたら `true`）。
///
/// ロック順は 取込行 → テーブル行（`FOR UPDATE`）→ バックフィルの進捗行（`FOR UPDATE`）。テーブル行の
/// `FOR UPDATE` は書込の `FOR KEY SHARE` と衝突するので、保持の前に台帳を読んだ書込は先に終わり、後の
/// 書込は `building` を見て write-through する（[`crate::index::state`] と同じ論法）。進捗行を押さえるので、
/// 走っているバックフィルのチャンクとも直列化される（保持の前に `active` へ切り替えたか、保持を見て
/// 切り替えないかのどちらか）。別の取込が保持していれば保持しない（同期で索引を書く）。
pub(super) async fn hold_index(
    conn: &mut PgConnection,
    tenant_id: &str,
    table: &DataTable,
    imp: &ImportRow,
) -> Result<bool, DataError> {
    let live: Option<i64> = sqlx::query_scalar(
        "SELECT schema_version FROM data_table \
         WHERE tenant_id = $1 AND id = $2 AND deleted_at IS NULL FOR UPDATE",
    )
    .bind(tenant_id)
    .bind(table.id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db)?;
    if live != Some(imp.schema_version) || index_held(conn, table.keys).await? {
        return Ok(false);
    }
    let _progress: Option<i32> = sqlx::query_scalar(
        "SELECT 1 FROM data_index_backfill WHERE tenant_int = $1 AND table_int = $2 FOR UPDATE",
    )
    .bind(table.keys.tenant_int)
    .bind(table.keys.table_int)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db)?;
    // 保持するのは**取込が値を書く列**の索引だけ（検証 D-5）。取込の行がどれも値を持たない列は、
    // 取込の行について索引行が要らない（null は索引行を持たない）ので active のまま使える
    // （以前は全列を保持し、取込の間はテーブル全体が並べ替え・絞り込みできなかった）。
    let written = written_keys(conn, imp).await?;
    for (field, t, state) in load_states(conn, table.keys).await? {
        let writes = table
            .schema
            .field_by_id(field)
            .is_some_and(|d| written.contains(&d.key));
        if state == IndexState::Active && writes && deferrable(&table.schema, field, t) {
            set_state(conn, table.keys, field, t, IndexState::Building).await?;
        }
    }
    sqlx::query(
        "UPDATE data_bulk_import SET index_hold = true, updated_at = now() \
         WHERE tenant_int = $1 AND id = $2",
    )
    .bind(imp.tenant_int)
    .bind(imp.id)
    .execute(&mut *conn)
    .await
    .map_err(map_db)?;
    tracing::info!(import_id = %imp.id, "索引を遅延構築します（値索引を building に保持）");
    Ok(true)
}

/// 取込の作成行（検証に合格した行）が null でない値を持つ列の参照名。
///
/// 作成行の本体は検証済みの値そのもの（`normalized` を `encode_record` して入れる）なので、ここに無い列は
/// 取込の行について索引行を作らない。更新行は 1 件の更新と同じく索引を write-through する。
async fn written_keys(
    conn: &mut PgConnection,
    imp: &ImportRow,
) -> Result<HashSet<String>, DataError> {
    let keys: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT e.key FROM data_bulk_staging s, jsonb_each(s.normalized) e \
         WHERE s.tenant_int = $1 AND s.import_id = $2 AND s.state = $3 AND s.op = $4 \
           AND jsonb_typeof(e.value) <> 'null'",
    )
    .bind(imp.tenant_int)
    .bind(imp.id)
    .bind(ST_VALID)
    .bind(OP_CREATE)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db)?;
    Ok(keys.into_iter().collect())
}

/// 取込の作業領域を片付ける（不合格・拒否の行はエラー報告のために残す）。
pub(super) async fn cleanup_staging(
    conn: &mut PgConnection,
    imp: &ImportRow,
) -> Result<(), DataError> {
    sqlx::query(
        "DELETE FROM data_bulk_staging \
         WHERE tenant_int = $1 AND import_id = $2 AND state IN ($3, $4, $5)",
    )
    .bind(imp.tenant_int)
    .bind(imp.id)
    .bind(ST_STAGED)
    .bind(ST_VALID)
    .bind(ST_APPLIED)
    .execute(&mut *conn)
    .await
    .map_err(map_db)?;
    sqlx::query("DELETE FROM data_bulk_key WHERE tenant_int = $1 AND import_id = $2")
        .bind(imp.tenant_int)
        .bind(imp.id)
        .execute(&mut *conn)
        .await
        .map_err(map_db)?;
    Ok(())
}

impl DataStore {
    /// 適用を 1 チャンク進める（最後のチャンクで `indexing` へ）。処理した行数を返す。
    #[allow(clippy::too_many_lines)] // 1 Tx の手順列（ロック・計画・作成・更新・進捗・監査）。分けると Tx 境界が追いにくい。
    pub(super) async fn apply_chunk(
        &self,
        tenant_id: &str,
        imp: &ImportRow,
        max: i64,
    ) -> Result<u64, DataError> {
        let ctx = imp.auth_context(tenant_id)?;
        let trace_id = imp.trace_id.as_deref();
        // 認可はチャンクごとに問い直す（剥奪は次のチャンクから効く）。
        let az = self.authz_memo();
        self.require_in(
            &az,
            &ctx,
            imp.table_id,
            Relation::Editor,
            AuditAction::DataRecordBulkApply,
            trace_id,
        )
        .await?;
        let table = self.fetch_live(&ctx, imp.table_id).await?;
        if table.schema_version != imp.schema_version {
            self.restart_validation(tenant_id, imp, table.schema_version)
                .await?;
            return Ok(0);
        }
        // 更新行は 1 件ずつ旧い索引行を消す。列統計の無いパーティションでは削除が列の索引行を全部
        // 読むので、Tx の前に統計を作る（#986・統計があれば 1 文で終わる）。
        analyze_index_partitions_without_stats(
            &self.pools,
            tenant_id,
            table.keys.tenant_int,
            table.keys.table_int,
        )
        .await;
        let mut tx = self
            .pools
            .for_tenant(tenant_id)
            .begin()
            .await
            .map_err(map_db)?;
        let cur = load_import(&mut tx, imp.tenant_int, imp.id, true)
            .await?
            .ok_or(DataError::NotFound)?;
        if cur.status != "applying"
            || cur.apply_cursor != imp.apply_cursor
            || cur.schema_version != imp.schema_version
        {
            tx.rollback().await.map_err(map_db)?;
            return Ok(0);
        }
        // 1 件の create と同じ計画（テーブル行の FOR KEY SHARE・保守する索引・unique の準備確認）。
        let plan = lock_plan(&mut tx, tenant_id, table.id, table.keys, imp.schema_version).await?;
        let rows: Vec<StagedRow> = sqlx::query_as(
            "SELECT row_no, record_id, state, normalized, op, expected_rev FROM data_bulk_staging \
             WHERE tenant_int = $1 AND import_id = $2 AND row_no > $3 AND row_no <= $4 \
             ORDER BY row_no LIMIT $5",
        )
        .bind(cur.tenant_int)
        .bind(cur.id)
        .bind(cur.apply_cursor)
        .bind(cur.validate_cursor)
        .bind(max)
        .fetch_all(&mut *tx)
        .await
        .map_err(map_db)?;
        let pending: Vec<Pending> = rows
            .iter()
            .filter(|r| r.2 == ST_VALID && r.4 == OP_CREATE)
            .filter_map(|(row_no, id, _, data, _, _)| {
                data.as_ref().map(|d| Pending {
                    row_no: *row_no,
                    id: *id,
                    data: d.0.clone(),
                })
            })
            .collect();
        // 作成行の列の書込権限を今の認可で問い直す（検証の後にロールを外されて読めなくなった列へは
        // 書かない・1 件の作成・更新行の update_locked と同じ単一点・PIT-19）。ロールの剥奪は
        // schema_version を変えないので、検証段の判定だけでは止まらない。
        let (pending, masked_rejects) = self
            .drop_masked_creates(&mut tx, &ctx, &az, &table, &cur, pending, trace_id)
            .await?;
        // 更新行（upsert）: 1 件の更新と同じ入口を行ごとのセーブポイントで通す（super::upsert）。
        let pending_updates: Vec<PendingUpdate> = rows
            .iter()
            .filter(|r| r.2 == ST_VALID && r.4 == OP_UPDATE)
            .filter_map(|(row_no, id, _, data, _, rev)| {
                data.as_ref().map(|d| PendingUpdate {
                    row_no: *row_no,
                    id: *id,
                    patch: d.0.clone(),
                    expected_rev: *rev,
                })
            })
            .collect();
        // 遅延構築中は、取込が building に保持した値索引の組を書かない（バックフィルが作る）。
        let deferred: HashSet<(FieldId, IndexTable)> = if cur.index_hold {
            load_states(&mut tx, table.keys)
                .await?
                .into_iter()
                .filter(|(f, t, s)| *s == IndexState::Building && deferrable(&table.schema, *f, *t))
                .map(|(f, t, _)| (f, t))
                .collect()
        } else {
            HashSet::new()
        };
        let defer = |f: FieldId, t: IndexTable| deferred.contains(&(f, t));
        let (applied, mut rejected) = self
            .apply_pending(&mut tx, &ctx, &table, &plan, &pending, &defer, trace_id)
            .await?;
        let (updated, update_rejected) = self
            .apply_updates(
                &mut tx,
                &ctx,
                &az,
                &table,
                &plan,
                &pending_updates,
                trace_id,
            )
            .await?;
        rejected.extend(update_rejected);
        rejected.extend(masked_rejects);
        rejected.sort_unstable_by_key(|r| r.0);
        let applied_rows: Vec<i64> = applied
            .iter()
            .map(|a| a.0)
            .chain(updated.iter().map(|u| u.0))
            .collect();
        write_applied(&mut tx, &cur, &applied_rows, &rejected).await?;
        let last = rows.last().map_or(cur.apply_cursor, |r| r.0);
        let finished = i64::try_from(rows.len()).unwrap_or(i64::MAX) < max;
        sqlx::query(
            "UPDATE data_bulk_import SET apply_cursor = $3, rows_applied = rows_applied + $4, \
               rows_rejected = rows_rejected + $5, \
               first_record_int = CASE WHEN $6::int8 IS NULL THEN first_record_int \
                 ELSE least(coalesce(first_record_int, $6::int8), $6::int8) END, \
               status = CASE WHEN $7 THEN 'indexing' ELSE status END, updated_at = now() \
             WHERE tenant_int = $1 AND id = $2",
        )
        .bind(cur.tenant_int)
        .bind(cur.id)
        .bind(last)
        .bind(i64::try_from(applied_rows.len()).unwrap_or(i64::MAX))
        .bind(i64::try_from(rejected.len()).unwrap_or(i64::MAX))
        // 遅延構築のバックフィルの開始位置は作成した行だけで決める（更新は索引を write-through する）。
        .bind(applied.iter().map(|a| a.1).min())
        .bind(finished)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        if !applied_rows.is_empty() || !rejected.is_empty() {
            // 実データ変更の監査（書込と同一 Tx・チャンクごとに 1 行。作成・更新したレコードの外部 ID を
            // 載せる。更新は 1 件ごとの監査も update_locked が同じ Tx で書く）。
            let ids: Vec<String> = applied.iter().map(|a| a.2.to_string()).collect();
            let updated_ids: Vec<String> = updated.iter().map(|u| u.1.to_string()).collect();
            self.record_audit_on(
                &mut tx,
                &ctx,
                AuditAction::DataRecordBulkApply,
                &table.id.to_string(),
                trace_id,
                json!({
                    "import_id": imp.id,
                    "created": applied.len(),
                    "updated": updated.len(),
                    "rejected": rejected.len(),
                    "record_ids": ids,
                    "updated_record_ids": updated_ids,
                }),
            )
            .await?;
        }
        tx.commit().await.map_err(map_db)?;
        Ok(rows.len() as u64)
    }

    /// 作成行のうち、今の認可で読めない（マスクされた）列へ書く行を外し、行エラーとして返す。
    ///
    /// 判定は検証段と同じく**取込の入力（payload）が名指しした列**で行う（FSM の初期状態など、入力に
    /// 無く検証が補った列では拒否しない）。文言は列名・値を含めない（検証段と同じ）。
    #[allow(clippy::too_many_arguments)]
    async fn drop_masked_creates(
        &self,
        tx: &mut PgConnection,
        ctx: &AuthContext,
        az: &crate::policy::memo::AuthzMemo<'_>,
        table: &DataTable,
        imp: &ImportRow,
        pending: Vec<Pending>,
        trace_id: Option<&str>,
    ) -> Result<(Vec<Pending>, Vec<(i64, String)>), DataError> {
        if pending.is_empty() {
            return Ok((pending, Vec::new()));
        }
        let masked = self.masked_fields(ctx, az, table).await?;
        if masked.is_empty() {
            return Ok((pending, Vec::new()));
        }
        let row_nos: Vec<i64> = pending.iter().map(|p| p.row_no).collect();
        let named: Vec<(i64, Vec<String>)> = sqlx::query_as(
            "SELECT row_no, ARRAY(SELECT jsonb_object_keys(payload)) FROM data_bulk_staging \
             WHERE tenant_int = $1 AND import_id = $2 AND row_no = ANY($3) \
               AND jsonb_typeof(payload) = 'object'",
        )
        .bind(imp.tenant_int)
        .bind(imp.id)
        .bind(&row_nos)
        .fetch_all(&mut *tx)
        .await
        .map_err(map_db)?;
        let denied: HashSet<i64> = named
            .into_iter()
            .filter(|(_, keys)| keys.iter().any(|k| masked.contains(k)))
            .map(|(n, _)| n)
            .collect();
        if denied.is_empty() {
            return Ok((pending, Vec::new()));
        }
        // 読めない列への書込はチャンクごとに 1 件の deny で残す（検証段と同じ）。
        self.audit_field_deny(
            ctx,
            table.id,
            crate::mask::WriteAudit::new(AuditAction::DataRecordBulkApply, trace_id),
        )
        .await;
        let (rejected, kept): (Vec<Pending>, Vec<Pending>) = pending
            .into_iter()
            .partition(|p| denied.contains(&p.row_no));
        let rejected = rejected
            .into_iter()
            .map(|p| (p.row_no, super::validate::MASKED_WRITE_MESSAGE.to_string()))
            .collect();
        Ok((kept, rejected))
    }

    /// 合格した行を適用する（セーブポイントでチャンクごと → 競合したら行ごと）。
    /// `(適用した (row_no, record_int, id), 拒否した (row_no, 文言))`。
    #[allow(clippy::too_many_arguments)]
    async fn apply_pending(
        &self,
        tx: &mut PgConnection,
        ctx: &AuthContext,
        table: &DataTable,
        plan: &IndexPlan,
        pending: &[Pending],
        defer: &(dyn Fn(FieldId, IndexTable) -> bool + Sync),
        trace_id: Option<&str>,
    ) -> Result<(Vec<Applied>, Vec<(i64, String)>), DataError> {
        let mut sp = tx.begin().await.map_err(map_db)?;
        match self
            .insert_created(&mut sp, ctx, table, plan, pending, defer, trace_id)
            .await
        {
            Ok(done) => {
                sp.commit().await.map_err(map_db)?;
                return Ok((done, Vec::new()));
            }
            Err(e) if e.is_conflict() || e.is_invalid() => {
                // 検証の後の並行書込と競合した行がある: チャンクを巻き戻し、行ごとにやり直す。
                sp.rollback().await.map_err(map_db)?;
                tracing::info!(error = %e, "チャンクの適用が競合したため行ごとに適用します");
            }
            Err(e) => return Err(e),
        }
        let mut applied = Vec::new();
        let mut rejected = Vec::new();
        for p in pending {
            let mut one = tx.begin().await.map_err(map_db)?;
            match self
                .insert_created(
                    &mut one,
                    ctx,
                    table,
                    plan,
                    std::slice::from_ref(p),
                    defer,
                    trace_id,
                )
                .await
            {
                Ok(done) => {
                    one.commit().await.map_err(map_db)?;
                    applied.extend(done);
                }
                Err(e) if e.is_conflict() || e.is_invalid() => {
                    one.rollback().await.map_err(map_db)?;
                    rejected.push((p.row_no, row_message(e)?));
                }
                Err(e) => return Err(e),
            }
        }
        Ok((applied, rejected))
    }

    /// 本体を 1 文で INSERT し、1 件の create と同じ入口で副作用を書く。`(row_no, record_int, id)` を返す。
    ///
    /// 外部 ID はステージング行が持つ（COPY 時に採番）ので、同じ行を二度適用しようとしても本体の
    /// 主キーで弾かれて 1 行のまま（`ON CONFLICT DO NOTHING`・副作用も書かない）。
    #[allow(clippy::too_many_arguments)]
    async fn insert_created(
        &self,
        conn: &mut PgConnection,
        ctx: &AuthContext,
        table: &DataTable,
        plan: &IndexPlan,
        rows: &[Pending],
        defer: &(dyn Fn(FieldId, IndexTable) -> bool + Sync),
        trace_id: Option<&str>,
    ) -> Result<Vec<Applied>, DataError> {
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<Uuid> = rows.iter().map(|p| p.id).collect();
        let bodies: Vec<String> = rows
            .iter()
            .map(|p| Value::Object(encode_record(&table.schema, &p.data)).to_string())
            .collect();
        let written: Vec<WrittenBody> = sqlx::query_as(
            "INSERT INTO data_record (id, tenant_id, table_id, org, data, owner, tenant_int, table_int) \
             SELECT u.id, $1, $2, $3, u.data::jsonb, $4, $5, $6 \
             FROM unnest($7::uuid[], $8::text[]) WITH ORDINALITY AS u(id, data, n) ORDER BY u.n \
             ON CONFLICT (tenant_int, table_int, id) DO NOTHING \
             RETURNING record_int, id, owner, created_at, updated_at",
        )
        .bind(&ctx.tenant_id)
        .bind(table.id)
        .bind(&ctx.org)
        .bind(&ctx.principal.id)
        .bind(table.keys.tenant_int)
        .bind(table.keys.table_int)
        .bind(&ids)
        .bind(&bodies)
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db)?;
        let by_id: HashMap<Uuid, &Pending> = rows.iter().map(|p| (p.id, p)).collect();
        let mut out = Vec::with_capacity(written.len());
        let mut recs = Vec::with_capacity(written.len());
        for (record_int, id, owner, created_at, updated_at) in &written {
            let Some(p) = by_id.get(id) else { continue };
            out.push((p.row_no, *record_int, *id));
            recs.push(CreatedRecord {
                record_int: *record_int,
                id: *id,
                owner,
                created_at: *created_at,
                updated_at: *updated_at,
                data: &p.data,
            });
        }
        self.after_records_created(conn, ctx, table, plan, &recs, defer, true, trace_id)
            .await?;
        Ok(out)
    }

    /// ④ 索引の仕上げ: 保持していれば外してバックフィルを始め、最後まで進めてから parity を検証して
    /// 取込を閉じる。処理したレコード数を返す。
    pub(super) async fn index_chunk(
        &self,
        tenant_id: &str,
        imp: &ImportRow,
    ) -> Result<u64, DataError> {
        if imp.index_hold {
            self.release_index_hold(tenant_id, imp).await?;
            return Ok(0);
        }
        match self
            .index_backfill_step(tenant_id, imp.table_id, BACKFILL_CHUNK)
            .await
        {
            Ok(step) if !step.done => return Ok(step.processed),
            Ok(_) | Err(DataError::NotFound) => {}
            Err(e) => return Err(e),
        }
        let report = match self.verify_index_parity(tenant_id, imp.table_id).await {
            Ok(r) => Some(r),
            Err(DataError::NotFound) => None,
            Err(e) => return Err(e),
        };
        let mismatches: Option<i32> = report.as_ref().map(|r| {
            i32::try_from(
                r.mismatches
                    .iter()
                    .map(|m| m.missing + m.extra)
                    .sum::<i64>(),
            )
            .unwrap_or(i32::MAX)
        });
        if mismatches.is_some_and(|m| m > 0) {
            tracing::error!(import_id = %imp.id, ?report, "一括取込の後の parity 検証で不一致");
        }
        let mut tx = self
            .pools
            .for_tenant(tenant_id)
            .begin()
            .await
            .map_err(map_db)?;
        let Some(cur) = load_import(&mut tx, imp.tenant_int, imp.id, true).await? else {
            return Ok(0);
        };
        if cur.status != "indexing" || cur.index_hold {
            tx.rollback().await.map_err(map_db)?;
            return Ok(0);
        }
        sqlx::query(
            "UPDATE data_bulk_import SET \
               status = CASE WHEN last_error IS NULL THEN 'done' ELSE 'failed' END, \
               parity_checked_at = CASE WHEN $3::int4 IS NULL THEN NULL ELSE now() END, \
               parity_mismatches = $3, finished_at = now(), updated_at = now() \
             WHERE tenant_int = $1 AND id = $2",
        )
        .bind(cur.tenant_int)
        .bind(cur.id)
        .bind(mismatches)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        cleanup_staging(&mut tx, &cur).await?;
        tx.commit().await.map_err(map_db)?;
        if cur.rows_valid >= ANALYZE_AFTER_ROWS {
            analyze_table_partitions(&self.pools, tenant_id, cur.tenant_int, cur.table_int).await;
        }
        Ok(0)
    }

    /// 値索引の保持を外し、取込で入れた範囲のバックフィルを始める。
    ///
    /// 開始位置は「取込で入れた最小の `record_int` の手前」。保持の時点で `active` だった組は、それより
    /// 前のレコードの行が揃っている（保持中の並行書込は write-through 済み）。別の理由（スキーマ改訂）の
    /// バックフィルが終わっていなければ先頭から（その組は前のレコードの行も欠けている）。
    async fn release_index_hold(&self, tenant_id: &str, imp: &ImportRow) -> Result<(), DataError> {
        let mut tx = self
            .pools
            .for_tenant(tenant_id)
            .begin()
            .await
            .map_err(map_db)?;
        let Some(cur) = load_import(&mut tx, imp.tenant_int, imp.id, true).await? else {
            return Ok(());
        };
        if !cur.index_hold {
            tx.rollback().await.map_err(map_db)?;
            return Ok(());
        }
        // ロック順は保持と同じ（取込行 → テーブル行 → 進捗行）。削除済みのテーブルも行は残る。
        let live: Option<i32> = sqlx::query_scalar(
            "SELECT 1 FROM data_table WHERE tenant_id = $1 AND id = $2 FOR UPDATE",
        )
        .bind(tenant_id)
        .bind(cur.table_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db)?;
        let keys = cur.keys();
        let progress: Option<String> = sqlx::query_scalar(
            "SELECT status FROM data_index_backfill \
             WHERE tenant_int = $1 AND table_int = $2 FOR UPDATE",
        )
        .bind(keys.tenant_int)
        .bind(keys.table_int)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db)?;
        sqlx::query(
            "UPDATE data_bulk_import SET index_hold = false, updated_at = now() \
             WHERE tenant_int = $1 AND id = $2",
        )
        .bind(cur.tenant_int)
        .bind(cur.id)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        if live.is_some() {
            let from = match (progress.as_deref(), cur.first_record_int) {
                (Some(s), _) if s != "done" => 0,
                (_, Some(first)) => first - 1,
                // 1 行も入らなかった: 作り直す行は無い（building を active へ戻すだけ）。
                (_, None) => i64::MAX,
            };
            start_backfill_from(&mut tx, keys, tenant_id, cur.table_id, from).await?;
        }
        tx.commit().await.map_err(map_db)?;
        Ok(())
    }
}

/// 適用・拒否の結果をステージング行へ書く。
async fn write_applied(
    conn: &mut PgConnection,
    imp: &ImportRow,
    applied: &[i64],
    rejected: &[(i64, String)],
) -> Result<(), DataError> {
    let mut row_nos: Vec<i64> = applied.to_vec();
    let mut states: Vec<i16> = applied.iter().map(|_| ST_APPLIED).collect();
    let mut errors: Vec<Option<&str>> = applied.iter().map(|_| None).collect();
    for (row_no, msg) in rejected {
        row_nos.push(*row_no);
        states.push(ST_REJECTED);
        errors.push(Some(msg.as_str()));
    }
    if row_nos.is_empty() {
        return Ok(());
    }
    sqlx::query(
        "UPDATE data_bulk_staging s SET state = u.st, error = u.e \
         FROM unnest($3::int8[], $4::int2[], $5::text[]) AS u(row_no, st, e) \
         WHERE s.tenant_int = $1 AND s.import_id = $2 AND s.row_no = u.row_no",
    )
    .bind(imp.tenant_int)
    .bind(imp.id)
    .bind(&row_nos)
    .bind(&states)
    .bind(&errors)
    .execute(&mut *conn)
    .await
    .map_err(map_db)?;
    Ok(())
}

/// 取込の後に Postgres の統計を取り直す行数の下限。
const ANALYZE_AFTER_ROWS: i64 = 1_000;

/// 取込で行が入ったパーティションの Postgres の統計を取り直す（best-effort・検証 2-D-P2-1）。
///
/// 取込の前に空に近い状態で ANALYZE されたパーティションは、取込の後も「行数 0」の統計のままで、Postgres が
/// 候補集めに値索引を使わない計画を選ぶ（10 万行で 1 秒超）。自動 ANALYZE を待たずに取り直す。ANALYZE は
/// 読み書きを止めない（`SHARE UPDATE EXCLUSIVE`）。SQL へ埋め込むのはカタログから引いた表名で、形を検証した
/// ものだけ（PIT-21）。
async fn analyze_table_partitions(
    pools: &storage::PoolRouter,
    tenant_id: &str,
    tenant_int: i32,
    table_int: i64,
) {
    let pool = pools.for_tenant(tenant_id);
    const PARENTS: [&str; 6] = [
        "data_record",
        "data_index_text",
        "data_index_num",
        "data_index_search",
        "data_link",
        "data_unique",
    ];
    let keys = TableKeys {
        tenant_int,
        table_int,
    };
    for parent in PARENTS {
        let rel = match partition_holding(pools, tenant_id, parent, keys).await {
            Ok(Some(r)) => r,
            Ok(None) => continue,
            Err(e) => {
                tracing::warn!(error = %e, parent, "取込の後の統計の取り直しの対象を引けません");
                continue;
            }
        };
        if let Err(e) = sqlx::query(&format!("ANALYZE {rel}")).execute(pool).await {
            tracing::warn!(error = %e, rel, "取込の後の統計の取り直しに失敗（自動 ANALYZE に任せる）");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FieldDef, FieldType, SYSTEM_FIELD_OWNER};

    #[test]
    fn only_user_value_indexes_are_deferred() {
        let mut title = FieldDef::new("title", FieldType::Text);
        title.id = 16;
        let mut total = FieldDef::new("total", FieldType::Formula);
        total.id = 17;
        let schema = TableSchema {
            fields: vec![title, total],
            status_field: None,
            row_policy: None,
            field_policy: vec![],
            aggregate_min_rows: None,
            fsm_ref: None,
            rag: None,
        };
        assert!(deferrable(&schema, 16, IndexTable::Text));
        assert!(deferrable(&schema, 16, IndexTable::Search));
        assert!(deferrable(&schema, 16, IndexTable::Num));
        assert!(
            !deferrable(&schema, 16, IndexTable::Unique),
            "unique は強制を空白にしない"
        );
        assert!(!deferrable(&schema, 16, IndexTable::Link));
        assert!(
            !deferrable(&schema, SYSTEM_FIELD_OWNER, IndexTable::Text),
            "擬似列は全件を走査できる駆動索引"
        );
        assert!(!deferrable(&schema, 17, IndexTable::Num), "材料化列");
        assert!(!deferrable(&schema, 99, IndexTable::Text), "定義の無い列");
    }
}
