//! 索引の自動昇格と降格のジョブ化（data-platform §10・PIT-72・Task 15.6）。
//!
//! # 昇格（未索引の列での絞り込み・並べ替え・ビューの保存が契機）
//!
//! - 403 / 422 で終わらせず、**2 万行未満のテーブルはその場でバックフィルして透過的に有効化**する
//!   （リクエストの中で `absent → building → active`）。それ以上は jobq のバックフィルを積み、
//!   完了まではその列での操作を「準備中」（409 `index_preparing`・進捗つき）にする。
//! - `building` の間は書込が write-through し、クエリには使わせない（[`super::state`]）。
//! - **テーブルあたりの索引列数に上限**（既定 20・擬似列とリンクを除く・テナント設定で変更可）。
//!   上限を超える自動昇格は 409 `index_approval_required` で止め、**owner の明示の承認**
//!   （`approve = true`）でだけ超えられる（無自覚な肥大化を防ぐ）。数値は SharePoint Online の
//!   挙動（索引 20 本・2 万件以上は裏で作成）を叩き台にした。
//! - 昇格は閲覧者でも起こせる（グリッドの並べ替えの UX）が、見えない列（マスク）は 403 のまま、
//!   閲覧者ごとの派生列は索引にできない（422 のまま）。昇格は監査に残す。
//!
//! # 降格（宣言から外れた索引）
//!
//! 大きなテーブルの索引行の削除はスキーマ改訂の Tx で行わない（長い Tx でテーブル行のロックを持ち
//! 続けない）。台帳を `absent` にしてクエリと書込から外し、行はジョブ（`purge`）がチャンクで消す。
//! 後片付けが終わるまで同じ列の再昇格は「準備中」。

use authz::AuthContext;
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use utoipa::ToSchema;
use uuid::Uuid;

use super::state::{field_id_i16, load_states};
use super::{IndexState, IndexTable};
use crate::model::{DataTable, FieldDef, FieldType, TableKeys, TableSchema};
use crate::store::DataStore;
use crate::{map_db, DataError};

/// これ未満の行数のテーブルは、昇格・降格をリクエストの中で同期に済ませる（SharePoint Online の 2 万件）。
pub const SYNC_INDEX_MAX_ROWS: i64 = 20_000;
/// テーブルあたりの索引列数の既定上限（擬似列・リンクを除く・PIT-72）。
pub const DEFAULT_MAX_INDEXED_FIELDS: usize = 20;
/// 降格の後片付けの 1 チャンクの行数。
const PURGE_CHUNK: i64 = 5_000;

/// 索引の状態（API の表現）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum IndexStatus {
    /// 索引が無い（その列では絞り込み・並べ替えできない）。
    Absent,
    /// 準備中（バックフィル中・降格の後片付け中）。完了までその列では使えない。
    Preparing,
    /// 使える。
    Active,
}

/// 昇格の結果。
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct IndexPromotion {
    pub field: String,
    pub status: IndexStatus,
    /// バックフィルの進捗（0〜1・準備中のとき）。
    pub progress: Option<f64>,
}

/// 列ごとの索引の状態。
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct FieldIndexStatus {
    pub field: String,
    pub status: IndexStatus,
}

/// テーブルの索引の一覧（管理画面・グリッドの「準備中」表示用）。件数・内部キーは含めない。
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct TableIndexOverview {
    /// 索引を宣言した列（擬似列・リンクを除く）。
    pub fields: Vec<FieldIndexStatus>,
    /// 上限の対象になる索引列の数。
    pub indexed_fields: usize,
    /// 上限（テナント設定・既定 20）。超える昇格は owner の承認が要る。
    pub max_indexed_fields: usize,
    /// 実行中・直近のバックフィルの進捗（0〜1）。無ければ `None`。
    pub backfill_progress: Option<f64>,
    /// バックフィルの状態（`running` / `done` / `failed`）。
    pub backfill_status: Option<String>,
}

/// 上限の対象になる索引列か（値索引を持つ通常列・材料化した派生列。リンクと対称列は数えない）。
pub(crate) fn counts_toward_cap(f: &FieldDef) -> bool {
    if f.is_reverse_link() || f.field_type == FieldType::RecordRef {
        return false;
    }
    if f.field_type.is_derived() {
        return f.is_materialized();
    }
    f.indexed || f.unique
}

/// 上限の対象になる索引列の数。
pub(crate) fn indexed_count(schema: &TableSchema) -> usize {
    schema
        .fields
        .iter()
        .filter(|f| counts_toward_cap(f))
        .count()
}

/// 自動昇格できる列か（通常の値列で、まだ値索引を持たない）。
pub(crate) fn promotable(f: &FieldDef) -> bool {
    !f.field_type.is_derived()
        && !f.is_reverse_link()
        && f.field_type != FieldType::RecordRef
        && !f.indexed
        && !f.unique
}

/// クエリが絞り込み・並べ替え・集計に使う列の参照名（自動昇格の契機・重複なし）。
pub(crate) fn query_fields(q: &crate::query::ir::DataQuery) -> Vec<String> {
    fn walk(c: &crate::query::ir::Condition, out: &mut Vec<String>) {
        use crate::query::ir::Condition;
        match c {
            Condition::All(v) | Condition::Any(v) => v.iter().for_each(|x| walk(x, out)),
            Condition::Not(x) => walk(x, out),
            Condition::Cmp(cmp) => out.push(cmp.field.clone()),
        }
    }
    let mut out = Vec::new();
    if let Some(f) = &q.filter {
        walk(f, &mut out);
    }
    out.extend(q.sort.iter().map(|k| k.field.clone()));
    out.extend(q.group_by.iter().cloned());
    out.extend(q.aggregate.iter().filter_map(|m| m.field.clone()));
    let mut seen = std::collections::HashSet::new();
    out.retain(|k| seen.insert(k.clone()));
    out
}

/// 値索引の表（number は num・それ以外は text）。
fn value_table(f: &FieldDef) -> IndexTable {
    if f.field_type == FieldType::Number {
        IndexTable::Num
    } else {
        IndexTable::Text
    }
}

/// 行数（`cap` で打ち切る・全件は数えない）。
async fn rows_upto(conn: &mut PgConnection, keys: TableKeys, cap: i64) -> Result<i64, DataError> {
    sqlx::query_scalar(
        "SELECT count(*) FROM (SELECT 1 FROM data_record \
         WHERE tenant_int = $1 AND table_int = $2 LIMIT $3) s",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .bind(cap)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db)
}

/// 降格の後片付けが残っているか。
pub(crate) async fn purge_pending(
    conn: &mut PgConnection,
    keys: TableKeys,
    field: crate::model::FieldId,
    table: IndexTable,
) -> Result<bool, DataError> {
    sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM data_index_purge \
         WHERE tenant_int = $1 AND table_int = $2 AND field_id = $3 AND index_table = $4)",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .bind(field_id_i16(field)?)
    .bind(table.code())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db)
}

/// 宣言から外れた (列, 索引表) の行を消す（台帳は呼び出し側が `absent` にした後）。
///
/// 小さいテーブルはこの Tx で消し、大きいテーブルは後片付けの台帳に載せてジョブで消す。
pub(crate) async fn demote_rows(
    conn: &mut PgConnection,
    keys: TableKeys,
    field: crate::model::FieldId,
    table: IndexTable,
) -> Result<(), DataError> {
    if rows_upto(conn, keys, SYNC_INDEX_MAX_ROWS).await? < SYNC_INDEX_MAX_ROWS {
        return delete_rows(conn, keys, field, table, None)
            .await
            .map(|_| ());
    }
    let high: i64 = sqlx::query_scalar(
        "SELECT coalesce(max(record_int), 0) FROM data_record \
         WHERE tenant_int = $1 AND table_int = $2",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db)?;
    sqlx::query(
        "INSERT INTO data_index_purge (tenant_int, table_int, field_id, index_table, high_water) \
         VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (tenant_int, table_int, field_id, index_table) \
         DO UPDATE SET high_water = greatest(data_index_purge.high_water, excluded.high_water)",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .bind(field_id_i16(field)?)
    .bind(table.code())
    .bind(high)
    .execute(&mut *conn)
    .await
    .map_err(map_db)?;
    let owner: Option<(String, Uuid)> = sqlx::query_as(
        "SELECT tenant_id, id FROM data_table WHERE tenant_int = $1 AND table_int = $2",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db)?;
    if let Some((tenant_id, table_id)) = owner {
        super::backfill::enqueue_op(conn, "purge", &tenant_id, table_id).await?;
    }
    tracing::info!(
        table_int = keys.table_int,
        "大きなテーブルの索引の降格をジョブに回しました"
    );
    Ok(())
}

/// 1 (列, 索引表) の行を消す（`limit` があればその件数まで）。消した件数。
async fn delete_rows(
    conn: &mut PgConnection,
    keys: TableKeys,
    field: crate::model::FieldId,
    table: IndexTable,
    limit: Option<(i64, i64)>,
) -> Result<u64, DataError> {
    let rel = table.relation();
    let rec = table.record_column();
    // 表名・列名は固定集合（IndexTable）からのみ埋め込む。値はバインド（PIT-21）。
    let text = match limit {
        None => {
            format!("DELETE FROM {rel} WHERE tenant_int = $1 AND table_int = $2 AND field_id = $3")
        }
        Some(_) => format!(
            "DELETE FROM {rel} WHERE tenant_int = $1 AND table_int = $2 AND field_id = $3 \
               AND ctid = ANY(ARRAY(SELECT ctid FROM {rel} \
                 WHERE tenant_int = $1 AND table_int = $2 AND field_id = $3 AND {rec} <= $4 \
                 LIMIT $5))"
        ),
    };
    let q = sqlx::query(&text)
        .bind(keys.tenant_int)
        .bind(keys.table_int)
        .bind(field_id_i16(field)?);
    let q = match limit {
        Some((high, n)) => q.bind(high).bind(n),
        None => q,
    };
    Ok(q.execute(&mut *conn).await.map_err(map_db)?.rows_affected())
}

impl DataStore {
    /// 列の索引を昇格する（自動昇格の本体・`approve` は上限超過の明示の承認＝owner のみ）。
    ///
    /// 2 万行未満のテーブルはこの呼び出しの中で有効化まで済ませる（`Active`）。それ以上は
    /// バックフィルを積んで `Preparing` を返す。既に索引があれば今の状態を返す（冪等）。
    pub async fn promote_index(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        field: &str,
        approve: bool,
        trace_id: Option<&str>,
    ) -> Result<IndexPromotion, DataError> {
        let az = self.authz_memo();
        let table = self
            .open_for_read(ctx, &az, table_id, "data.index.promote", trace_id)
            .await?;
        let f = table.schema.field(field).ok_or_else(|| {
            DataError::Invalid(format!("フィールド '{field}' はスキーマに存在しません"))
        })?;
        if self.masked_fields(ctx, &az, &table).await?.contains(field) {
            return Err(DataError::Forbidden);
        }
        if !promotable(f) {
            if f.field_type.is_derived() && !f.is_materialized() {
                return Err(DataError::Invalid(format!(
                    "派生フィールド '{field}' は索引にできません（閲覧者ごと・時刻依存の値）"
                )));
            }
            if f.is_reverse_link() {
                return Err(DataError::Invalid(format!(
                    "対称列 '{field}' は索引にできません（リンク元の列で絞り込んでください）"
                )));
            }
            return self.field_index_state(&ctx.tenant_id, &table, f).await;
        }
        let cap = self.max_indexed_fields_for(&ctx.tenant_id).await?;
        let count = indexed_count(&table.schema);
        if count + 1 > cap {
            if !approve {
                return Err(DataError::IndexApprovalRequired {
                    field: field.to_string(),
                    indexed: count,
                    cap,
                });
            }
            // 上限を超える索引は owner の明示の承認でだけ作る。
            self.require_in(
                &az,
                ctx,
                table_id,
                authz::Relation::Owner,
                "data.index.approve",
                trace_id,
            )
            .await?;
        }
        let (sync, building) = self
            .promote_in_tx(ctx, &table, f.id, approve, trace_id)
            .await?;
        self.schema_cache.invalidate_table(&ctx.tenant_id, table_id);
        if building && sync {
            // 小さいテーブルはこの場でバックフィルを終えて有効化する（透過的な有効化）。
            self.backfill_table_index(&ctx.tenant_id, table_id).await?;
        }
        let table = self.fetch_live(ctx, table_id).await?;
        let f = table
            .schema
            .field(field)
            .ok_or_else(|| DataError::Internal("昇格した列が見つかりません".into()))?;
        self.field_index_state(&ctx.tenant_id, &table, f).await
    }

    /// 昇格の Tx（テーブル行を `FOR UPDATE`・宣言と台帳・監査）。`(同期で作れる, building になった)`。
    async fn promote_in_tx(
        &self,
        ctx: &AuthContext,
        table: &DataTable,
        field: crate::model::FieldId,
        approved: bool,
        trace_id: Option<&str>,
    ) -> Result<(bool, bool), DataError> {
        let mut tx = self
            .pools
            .for_tenant(&ctx.tenant_id)
            .begin()
            .await
            .map_err(map_db)?;
        let schema: Option<sqlx::types::Json<TableSchema>> = sqlx::query_scalar(
            "SELECT schema FROM data_table \
             WHERE tenant_id = $1 AND id = $2 AND deleted_at IS NULL FOR UPDATE",
        )
        .bind(&ctx.tenant_id)
        .bind(table.id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db)?;
        let mut schema = schema.ok_or(DataError::NotFound)?.0;
        let Some(def) = schema.fields.iter_mut().find(|d| d.id == field) else {
            return Err(DataError::NotFound);
        };
        if !promotable(def) {
            // 並行の昇格が先に済ませた。
            tx.rollback().await.map_err(map_db)?;
            return Ok((true, false));
        }
        let key = def.key.clone();
        let vt = value_table(def);
        if purge_pending(&mut tx, table.keys, field, vt).await? {
            return Err(DataError::IndexPreparing {
                field: key,
                progress: None,
            });
        }
        def.indexed = true;
        sqlx::query(
            "UPDATE data_table SET schema = $3, schema_version = schema_version + 1, \
               updated_at = now() WHERE tenant_id = $1 AND id = $2",
        )
        .bind(&ctx.tenant_id)
        .bind(table.id)
        .bind(sqlx::types::Json(&schema))
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        let rows = rows_upto(&mut tx, table.keys, SYNC_INDEX_MAX_ROWS).await?;
        let building = super::state::sync_ledger(&mut tx, table.keys, &schema, rows > 0).await?;
        if building {
            super::backfill::start_backfill(&mut tx, table.keys, &ctx.tenant_id, table.id).await?;
        }
        self.record_audit_on(
            &mut tx,
            ctx,
            "data.index.promote",
            &table.id.to_string(),
            trace_id,
            serde_json::json!({ "field": key, "approved_over_cap": approved, "sync": rows < SYNC_INDEX_MAX_ROWS }),
        )
        .await?;
        tx.commit().await.map_err(map_db)?;
        Ok((rows < SYNC_INDEX_MAX_ROWS, building))
    }

    /// 1 列の索引の状態（台帳・バックフィルの進捗・降格の後片付け）。
    async fn field_index_state(
        &self,
        tenant_id: &str,
        table: &DataTable,
        f: &FieldDef,
    ) -> Result<IndexPromotion, DataError> {
        let mut conn = self
            .pools
            .for_tenant(tenant_id)
            .acquire()
            .await
            .map_err(map_db)?;
        let ledger = load_states(&mut conn, table.keys).await?;
        let want = value_table(f);
        let st = ledger
            .iter()
            .find(|(fid, t, _)| *fid == f.id && *t == want)
            .map_or(IndexState::Absent, |s| s.2);
        let status = match st {
            IndexState::Active => IndexStatus::Active,
            IndexState::Building => IndexStatus::Preparing,
            IndexState::Absent if purge_pending(&mut conn, table.keys, f.id, want).await? => {
                IndexStatus::Preparing
            }
            IndexState::Absent => IndexStatus::Absent,
        };
        let progress = if status == IndexStatus::Preparing {
            backfill_progress(&mut conn, table.keys).await?
        } else {
            None
        };
        Ok(IndexPromotion {
            field: f.key.clone(),
            status,
            progress,
        })
    }

    /// テーブルの索引の一覧（viewer・件数や内部キーは返さない）。
    pub async fn table_indexes(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<TableIndexOverview, DataError> {
        let az = self.authz_memo();
        let table = self
            .open_for_read(ctx, &az, table_id, "data.index.list", trace_id)
            .await?;
        let masked = self.masked_fields(ctx, &az, &table).await?;
        let mut fields = Vec::new();
        for f in table.schema.fields.iter().filter(|f| counts_toward_cap(f)) {
            if masked.contains(&f.key) {
                continue; // 見えない列は索引の有無も出さない
            }
            let s = self.field_index_state(&ctx.tenant_id, &table, f).await?;
            fields.push(FieldIndexStatus {
                field: s.field,
                status: s.status,
            });
        }
        let mut conn = self
            .pools
            .for_tenant(&ctx.tenant_id)
            .acquire()
            .await
            .map_err(map_db)?;
        let backfill: Option<String> = sqlx::query_scalar(
            "SELECT status FROM data_index_backfill WHERE tenant_int = $1 AND table_int = $2",
        )
        .bind(table.keys.tenant_int)
        .bind(table.keys.table_int)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db)?;
        Ok(TableIndexOverview {
            fields,
            indexed_fields: indexed_count(&table.schema),
            max_indexed_fields: self.max_indexed_fields_for(&ctx.tenant_id).await?,
            backfill_progress: backfill_progress(&mut conn, table.keys).await?,
            backfill_status: backfill,
        })
    }

    /// 降格の後片付けを 1 チャンク進める（ジョブ `purge`）。残りが無ければ `true`。
    pub async fn index_purge_step(
        &self,
        tenant_id: &str,
        table_id: Uuid,
    ) -> Result<bool, DataError> {
        let Some(keys) = self.table_keys_of(tenant_id, table_id).await? else {
            return Ok(true);
        };
        let mut tx = self
            .pools
            .for_tenant(tenant_id)
            .begin()
            .await
            .map_err(map_db)?;
        let pending: Vec<(i16, i16, i64)> = sqlx::query_as(
            "SELECT field_id, index_table, high_water FROM data_index_purge \
             WHERE tenant_int = $1 AND table_int = $2 ORDER BY field_id, index_table \
             LIMIT 1 FOR UPDATE",
        )
        .bind(keys.tenant_int)
        .bind(keys.table_int)
        .fetch_all(&mut *tx)
        .await
        .map_err(map_db)?;
        let Some((f, t, high)) = pending.first().copied() else {
            tx.commit().await.map_err(map_db)?;
            return Ok(true);
        };
        let (Ok(field), Some(table)) =
            (crate::model::FieldId::try_from(f), IndexTable::from_code(t))
        else {
            return Err(DataError::Internal("降格の台帳が不正です".into()));
        };
        let n = delete_rows(&mut tx, keys, field, table, Some((high, PURGE_CHUNK))).await?;
        if n == 0 {
            sqlx::query(
                "DELETE FROM data_index_purge WHERE tenant_int = $1 AND table_int = $2 \
                   AND field_id = $3 AND index_table = $4",
            )
            .bind(keys.tenant_int)
            .bind(keys.table_int)
            .bind(f)
            .bind(t)
            .execute(&mut *tx)
            .await
            .map_err(map_db)?;
        }
        tx.commit().await.map_err(map_db)?;
        Ok(false)
    }

    /// クエリの未索引の列を自動昇格する（グリッドの並べ替え・絞り込み・ビューの保存）。昇格して
    /// 使えるようになった列があれば `true`（呼び出し側がスキーマを読み直す）。大きなテーブルは
    /// 準備中（409）、上限超過は承認待ち（409）で返る。マスク列・派生列・未知の列には触らない
    /// （束縛が従来どおり 403 / 422 にする）。
    pub(crate) async fn auto_promote_for(
        &self,
        ctx: &AuthContext,
        table: &DataTable,
        masked: &std::collections::HashSet<String>,
        q: &crate::query::ir::DataQuery,
        trace_id: Option<&str>,
    ) -> Result<bool, DataError> {
        let mut promoted = false;
        for key in query_fields(q) {
            let Some(f) = table.schema.field(&key) else {
                continue;
            };
            if masked.contains(&key) || !promotable(f) {
                continue;
            }
            let p = self
                .promote_index(ctx, table.id, &key, false, trace_id)
                .await?;
            if p.status != IndexStatus::Active {
                return Err(DataError::IndexPreparing {
                    field: p.field,
                    progress: p.progress,
                });
            }
            promoted = true;
        }
        Ok(promoted)
    }

    /// テナントの索引列数の上限（設定が無ければ既定 20）。
    pub(crate) async fn max_indexed_fields_for(&self, tenant_id: &str) -> Result<usize, DataError> {
        let v: Option<Option<i32>> = sqlx::query_scalar(
            "SELECT max_indexed_fields FROM data_tenant_settings WHERE tenant_id = $1",
        )
        .bind(tenant_id)
        .fetch_optional(self.pools.for_tenant(tenant_id))
        .await
        .map_err(map_db)?;
        Ok(v.flatten()
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(DEFAULT_MAX_INDEXED_FIELDS))
    }
}

/// バックフィルの進捗（0〜1・行が無ければ `None`）。
#[allow(clippy::cast_precision_loss)] // 進捗の比（表示用・件数は 2^52 未満）。
pub(crate) async fn backfill_progress(
    conn: &mut PgConnection,
    keys: TableKeys,
) -> Result<Option<f64>, DataError> {
    let row: Option<(String, i64, i64)> = sqlx::query_as(
        "SELECT status, rows_done, rows_total FROM data_index_backfill \
         WHERE tenant_int = $1 AND table_int = $2",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db)?;
    Ok(row.map(|(status, done, total)| {
        if status == "done" || total <= 0 {
            1.0
        } else {
            (done as f64 / total as f64).clamp(0.0, 1.0)
        }
    }))
}
