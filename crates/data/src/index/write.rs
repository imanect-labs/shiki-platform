//! 書込経路の索引更新（write-through・data-platform §3.6）。
//!
//! レコード書込と**同一トランザクション**で、索引状態が `building` / `active` の
//! (フィールド, 索引テーブル) の行だけを書き換える。
//!
//! - **差分更新**: update / FSM 遷移は `FieldPatch`（v1 の差分）に載ったフィールドの索引行だけを触る。
//!   変わらないフィールドの索引行は 1 行も書き換えない（擬似列 `updated_at` だけは毎回動く）。
//! - create は全フィールド＋擬似列 3 行、delete はそのレコードの全索引行を消す。
//! - バックフィル（[`super::backfill`]）は `building` の組だけを作り直す（冪等）。
//!
//! 値は全てバインド。SQL へ埋め込むのは固定の表名（[`IndexTable::relation`]）だけ（PIT-21）。

use chrono::{DateTime, Utc};
use serde_json::{Map, Value};
use sqlx::PgConnection;

use super::link::{replace_forward, LinkResolver};
use super::normalize::{field_keys, format_timestamp, text_key, FieldKeys};
use super::state::{field_id_i16, IndexPlan};
use super::IndexTable;
use crate::model::{
    FieldId, FieldPatch, TableSchema, FIRST_USER_FIELD_ID, SYSTEM_FIELD_CREATED_AT,
    SYSTEM_FIELD_OWNER, SYSTEM_FIELD_UPDATED_AT,
};
use crate::{map_db, DataError};

/// リンク列 1 つ分の参照先（ord, 参照先の外部 ID）。
type LinkTargets = Vec<(i16, uuid::Uuid)>;

/// 索引行を書くレコード（外部形 `key → 値` の保存値と擬似列）。
pub(crate) struct IndexedRecord<'a> {
    pub record_int: i64,
    pub owner: &'a str,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// 外部形（`key → 値`）の保存値（書込検証で正規化済み）。
    pub data: &'a Map<String, Value>,
}

/// 既存の索引行の扱い。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// 新規レコード（既存行が無いので消さない）。
    Fresh,
    /// 書込: 消してから入れる。unique は旧値の行を消してから素の INSERT（重複は 409）。
    Replace,
    /// バックフィル: 消してから入れる。unique は「同じレコードの行なら上書き」で冪等にする。
    Rebuild,
}

/// 1 フィールド × 1 索引テーブルの書換え。
struct Op<'v> {
    field: FieldId,
    table: IndexTable,
    old: Option<&'v Value>,
    new: Option<&'v Value>,
}

/// 書込 Tx 上の索引ライタ（[`super::state::lock_plan`] で得た計画に従う）。
pub(crate) struct IndexWriter<'a> {
    pub plan: &'a IndexPlan,
    pub schema: &'a TableSchema,
    /// 参照先テーブルの解決に使う（同一テナント内）。
    pub tenant_id: &'a str,
    pub max_multi_values: usize,
}

impl IndexWriter<'_> {
    /// create: 保守対象の全フィールド＋擬似列 3 行を書く。
    pub(crate) async fn on_create(
        &self,
        conn: &mut PgConnection,
        rec: &IndexedRecord<'_>,
    ) -> Result<(), DataError> {
        let sys = SystemValues::of(rec);
        let mut ops = Vec::new();
        for (field, tables) in self.plan.fields() {
            let new = self.value_of(field, rec.data, &sys);
            for t in tables {
                ops.push(Op {
                    field,
                    table: *t,
                    old: None,
                    new,
                });
            }
        }
        self.apply(conn, rec.record_int, &ops, Mode::Fresh).await
    }

    /// update / FSM 遷移: 差分（`patches`・外部形 key）に載ったフィールドと `updated_at` だけを書き換える。
    pub(crate) async fn on_update(
        &self,
        conn: &mut PgConnection,
        record_int: i64,
        patches: &[FieldPatch],
        updated_at: DateTime<Utc>,
    ) -> Result<(), DataError> {
        let updated = Value::String(format_timestamp(updated_at));
        let mut ops = Vec::new();
        for p in patches {
            let Some(def) = self.schema.field(&p.field) else {
                continue;
            };
            for t in self.plan.tables_for(def.id) {
                ops.push(Op {
                    field: def.id,
                    table: *t,
                    old: non_null(&p.old),
                    new: non_null(&p.new),
                });
            }
        }
        for t in self.plan.tables_for(SYSTEM_FIELD_UPDATED_AT) {
            ops.push(Op {
                field: SYSTEM_FIELD_UPDATED_AT,
                table: *t,
                old: None,
                new: Some(&updated),
            });
        }
        self.apply(conn, record_int, &ops, Mode::Replace).await
    }

    /// delete: そのレコードの索引行を全て消す（`old` は外部形の保存値・unique 行の特定に使う）。
    pub(crate) async fn on_delete(
        &self,
        conn: &mut PgConnection,
        record_int: i64,
        old: &Map<String, Value>,
    ) -> Result<(), DataError> {
        let mut ops = Vec::new();
        for (field, tables) in self.plan.fields() {
            let old_value = self
                .schema
                .field_by_id(field)
                .and_then(|d| old.get(&d.key))
                .and_then(non_null);
            for t in tables {
                ops.push(Op {
                    field,
                    table: *t,
                    old: old_value,
                    new: None,
                });
            }
        }
        self.apply(conn, record_int, &ops, Mode::Replace).await
    }

    /// バックフィル: 指定の (フィールド, 索引テーブル) だけを本体から作り直す（冪等）。
    pub(crate) async fn rebuild(
        &self,
        conn: &mut PgConnection,
        rec: &IndexedRecord<'_>,
        targets: &[(FieldId, IndexTable)],
    ) -> Result<(), DataError> {
        let sys = SystemValues::of(rec);
        let ops: Vec<Op<'_>> = targets
            .iter()
            .map(|(field, table)| Op {
                field: *field,
                table: *table,
                old: None,
                new: self.value_of(*field, rec.data, &sys),
            })
            .collect();
        self.apply(conn, rec.record_int, &ops, Mode::Rebuild).await
    }

    fn value_of<'v>(
        &self,
        field: FieldId,
        data: &'v Map<String, Value>,
        sys: &'v SystemValues,
    ) -> Option<&'v Value> {
        match field {
            SYSTEM_FIELD_OWNER => Some(&sys.owner),
            SYSTEM_FIELD_CREATED_AT => Some(&sys.created_at),
            SYSTEM_FIELD_UPDATED_AT => Some(&sys.updated_at),
            _ => self
                .schema
                .field_by_id(field)
                .and_then(|d| data.get(&d.key))
                .and_then(non_null),
        }
    }

    /// 1 フィールドの索引キー（擬似列は値そのものを text キーに）。
    fn keys_of(
        &self,
        field: FieldId,
        table: IndexTable,
        v: Option<&Value>,
    ) -> Result<Option<FieldKeys>, DataError> {
        let Some(v) = v else { return Ok(None) };
        if field < FIRST_USER_FIELD_ID {
            let s = v.as_str().ok_or_else(|| {
                DataError::Internal(format!("擬似列 {field} の値が文字列ではありません"))
            })?;
            return Ok(Some(FieldKeys::Text(vec![text_key(0, s)])));
        }
        let Some(def) = self.schema.field_by_id(field) else {
            return Ok(None);
        };
        field_keys(def, table, v, self.max_multi_values)
    }

    async fn apply(
        &self,
        conn: &mut PgConnection,
        record_int: i64,
        ops: &[Op<'_>],
        mode: Mode,
    ) -> Result<(), DataError> {
        let mut batch = Batch::default();
        let mut links: Vec<(FieldId, LinkTargets)> = Vec::new();
        for op in ops {
            // 定義の無いユーザー列（台帳とスキーマの食い違い）は触らない（消しもしない）。
            if op.field >= FIRST_USER_FIELD_ID && self.schema.field_by_id(op.field).is_none() {
                continue;
            }
            let fid = field_id_i16(op.field)?;
            if op.table != IndexTable::Link && op.table != IndexTable::Unique {
                batch.delete(op.table, fid);
            }
            match (op.table, self.keys_of(op.field, op.table, op.new)?) {
                (IndexTable::Link, keys) => {
                    let targets = match keys {
                        Some(FieldKeys::Link(t)) => t,
                        _ => Vec::new(),
                    };
                    links.push((op.field, targets));
                }
                (IndexTable::Unique, keys) => {
                    if mode == Mode::Replace {
                        if let Some(FieldKeys::Unique(old)) =
                            self.keys_of(op.field, op.table, op.old)?
                        {
                            batch.unique_delete.push((fid, old));
                        }
                    }
                    if let Some(FieldKeys::Unique(norm)) = keys {
                        batch.unique_insert.push((fid, norm));
                    }
                }
                (_, Some(FieldKeys::Text(rows))) => {
                    for r in rows {
                        batch.text.push((fid, r.ord, r.val, r.truncated));
                    }
                }
                (_, Some(FieldKeys::Num(rows))) => {
                    for (ord, n) in rows {
                        batch.num.push((fid, ord, n));
                    }
                }
                (_, Some(FieldKeys::Search(rows))) => {
                    for (ord, s) in rows {
                        batch.search.push((fid, ord, s));
                    }
                }
                (_, _) => {}
            }
        }
        let keys = self.plan.keys;
        if mode != Mode::Fresh {
            for (table, fields) in &batch.deletes {
                delete_rows(conn, keys, *table, fields, record_int).await?;
            }
            delete_unique(conn, keys, &batch.unique_delete, record_int).await?;
        }
        insert_text(conn, keys, record_int, &batch.text).await?;
        insert_num(conn, keys, record_int, &batch.num).await?;
        insert_search(conn, keys, record_int, &batch.search).await?;
        insert_unique(
            conn,
            keys,
            record_int,
            &batch.unique_insert,
            mode == Mode::Rebuild,
        )
        .await?;
        let mut resolver = LinkResolver::default();
        for (field, targets) in links {
            let ref_table = self
                .schema
                .field_by_id(field)
                .and_then(|d| d.ref_table)
                .ok_or_else(|| DataError::Internal(format!("リンク列 {field} の参照先が不明")))?;
            let mut edges = Vec::with_capacity(targets.len());
            for (ord, id) in targets {
                if let Some((dt, dr)) = resolver
                    .resolve(conn, self.tenant_id, keys.tenant_int, ref_table, id)
                    .await?
                {
                    edges.push((ord, dt, dr));
                }
            }
            replace_forward(conn, keys, field, record_int, &edges).await?;
        }
        Ok(())
    }
}

fn non_null(v: &Value) -> Option<&Value> {
    (!v.is_null()).then_some(v)
}

/// 擬似列の値（text キーの元）。
struct SystemValues {
    owner: Value,
    created_at: Value,
    updated_at: Value,
}

impl SystemValues {
    fn of(rec: &IndexedRecord<'_>) -> Self {
        SystemValues {
            owner: Value::String(rec.owner.to_string()),
            created_at: Value::String(format_timestamp(rec.created_at)),
            updated_at: Value::String(format_timestamp(rec.updated_at)),
        }
    }
}

/// 1 レコード分の索引行（表ごとにまとめて 1 文で書く）。
#[derive(Default)]
struct Batch {
    deletes: Vec<(IndexTable, Vec<i16>)>,
    text: Vec<(i16, i16, String, bool)>,
    num: Vec<(i16, i16, String)>,
    search: Vec<(i16, i16, String)>,
    unique_delete: Vec<(i16, String)>,
    unique_insert: Vec<(i16, String)>,
}

impl Batch {
    fn delete(&mut self, table: IndexTable, field: i16) {
        match self.deletes.iter_mut().find(|(t, _)| *t == table) {
            Some((_, fields)) if !fields.contains(&field) => fields.push(field),
            Some(_) => {}
            None => self.deletes.push((table, vec![field])),
        }
    }
}

async fn delete_rows(
    conn: &mut PgConnection,
    keys: crate::model::TableKeys,
    table: IndexTable,
    fields: &[i16],
    record_int: i64,
) -> Result<(), DataError> {
    // PK (tenant_int, table_int, field_id, record_int, ord) の前方一致で引ける形。
    let sql = format!(
        "DELETE FROM {} WHERE tenant_int = $1 AND table_int = $2 \
           AND field_id = ANY($3::int2[]) AND {} = $4",
        table.relation(),
        table.record_column()
    );
    sqlx::query(&sql)
        .bind(keys.tenant_int)
        .bind(keys.table_int)
        .bind(fields)
        .bind(record_int)
        .execute(&mut *conn)
        .await
        .map_err(map_db)?;
    Ok(())
}

async fn insert_text(
    conn: &mut PgConnection,
    keys: crate::model::TableKeys,
    record_int: i64,
    rows: &[(i16, i16, String, bool)],
) -> Result<(), DataError> {
    if rows.is_empty() {
        return Ok(());
    }
    let fields: Vec<i16> = rows.iter().map(|r| r.0).collect();
    let ords: Vec<i16> = rows.iter().map(|r| r.1).collect();
    let vals: Vec<&str> = rows.iter().map(|r| r.2.as_str()).collect();
    let truncs: Vec<bool> = rows.iter().map(|r| r.3).collect();
    sqlx::query(
        "INSERT INTO data_index_text \
           (tenant_int, table_int, field_id, record_int, ord, val, truncated) \
         SELECT $1, $2, u.f, $3, u.o, u.v, u.t \
         FROM unnest($4::int2[], $5::int2[], $6::text[], $7::bool[]) AS u(f, o, v, t)",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .bind(record_int)
    .bind(&fields)
    .bind(&ords)
    .bind(&vals)
    .bind(&truncs)
    .execute(&mut *conn)
    .await
    .map_err(map_db)?;
    Ok(())
}

async fn insert_num(
    conn: &mut PgConnection,
    keys: crate::model::TableKeys,
    record_int: i64,
    rows: &[(i16, i16, String)],
) -> Result<(), DataError> {
    if rows.is_empty() {
        return Ok(());
    }
    let fields: Vec<i16> = rows.iter().map(|r| r.0).collect();
    let ords: Vec<i16> = rows.iter().map(|r| r.1).collect();
    let vals: Vec<&str> = rows.iter().map(|r| r.2.as_str()).collect();
    sqlx::query(
        "INSERT INTO data_index_num (tenant_int, table_int, field_id, record_int, ord, val) \
         SELECT $1, $2, u.f, $3, u.o, u.v::numeric \
         FROM unnest($4::int2[], $5::int2[], $6::text[]) AS u(f, o, v)",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .bind(record_int)
    .bind(&fields)
    .bind(&ords)
    .bind(&vals)
    .execute(&mut *conn)
    .await
    .map_err(map_db)?;
    Ok(())
}

async fn insert_search(
    conn: &mut PgConnection,
    keys: crate::model::TableKeys,
    record_int: i64,
    rows: &[(i16, i16, String)],
) -> Result<(), DataError> {
    if rows.is_empty() {
        return Ok(());
    }
    let fields: Vec<i16> = rows.iter().map(|r| r.0).collect();
    let ords: Vec<i16> = rows.iter().map(|r| r.1).collect();
    let vals: Vec<&str> = rows.iter().map(|r| r.2.as_str()).collect();
    sqlx::query(
        "INSERT INTO data_index_search (tenant_int, table_int, field_id, record_int, ord, val) \
         SELECT $1, $2, u.f, $3, u.o, u.v \
         FROM unnest($4::int2[], $5::int2[], $6::text[]) AS u(f, o, v)",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .bind(record_int)
    .bind(&fields)
    .bind(&ords)
    .bind(&vals)
    .execute(&mut *conn)
    .await
    .map_err(map_db)?;
    Ok(())
}

async fn delete_unique(
    conn: &mut PgConnection,
    keys: crate::model::TableKeys,
    rows: &[(i16, String)],
    record_int: i64,
) -> Result<(), DataError> {
    if rows.is_empty() {
        return Ok(());
    }
    let fields: Vec<i16> = rows.iter().map(|r| r.0).collect();
    let norms: Vec<&str> = rows.iter().map(|r| r.1.as_str()).collect();
    // 他レコードの行は消さない（record_int も一致させる）。
    sqlx::query(
        "DELETE FROM data_unique WHERE tenant_int = $1 AND table_int = $2 \
           AND (field_id, val_norm) IN (SELECT * FROM unnest($3::int2[], $4::text[])) \
           AND record_int = $5",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .bind(&fields)
    .bind(&norms)
    .bind(record_int)
    .execute(&mut *conn)
    .await
    .map_err(map_db)?;
    Ok(())
}

async fn insert_unique(
    conn: &mut PgConnection,
    keys: crate::model::TableKeys,
    record_int: i64,
    rows: &[(i16, String)],
    idempotent: bool,
) -> Result<(), DataError> {
    if rows.is_empty() {
        return Ok(());
    }
    let fields: Vec<i16> = rows.iter().map(|r| r.0).collect();
    let norms: Vec<&str> = rows.iter().map(|r| r.1.as_str()).collect();
    // バックフィルは同じレコードの行なら上書き（冪等）、他レコードの行と衝突したら 0 行＝重複。
    let conflict = if idempotent {
        " ON CONFLICT (tenant_int, table_int, field_id, val_norm) \
          DO UPDATE SET record_int = excluded.record_int \
          WHERE du.record_int = excluded.record_int"
    } else {
        ""
    };
    let sql = format!(
        "INSERT INTO data_unique AS du (tenant_int, table_int, field_id, val_norm, record_int) \
         SELECT $1, $2, u.f, u.v, $3 FROM unnest($4::int2[], $5::text[]) AS u(f, v){conflict}"
    );
    let n = sqlx::query(&sql)
        .bind(keys.tenant_int)
        .bind(keys.table_int)
        .bind(record_int)
        .bind(&fields)
        .bind(&norms)
        .execute(&mut *conn)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(db) if db.is_unique_violation() => {
                DataError::Conflict("unique 制約に違反しています".into())
            }
            _ => map_db(e),
        })?
        .rows_affected();
    if n != rows.len() as u64 {
        return Err(DataError::Conflict("unique 制約に違反しています".into()));
    }
    Ok(())
}
