//! リンク値の保存形の移行（v1 の `record_ref` の単一 UUID 文字列 → 配列・data-platform §7.1 / §13 M1）。
//!
//! ```text
//! JSONB: {"f17": "6F1C…"}  →  {"f17": ["6f1c…"]}      （正準形の UUID・要素 1 件）
//! ```
//!
//! # 移行中の正しさ
//!
//! - **読取**は形を問わない。保存値の読み出し口（[`crate::field_key::decode_record`]）が文字列を要素 1 件の
//!   配列へ寄せ、本体経路の SQL（[`crate::query::v1`]）も文字列を要素 1 件として評価する。
//!   索引（`data_link` / `data_unique`）の導出もどちらの形から作っても同じキーになる。
//! - **書込**は常に配列を書く（[`crate::validate`]）。更新は読み出した完全形を書き戻すので、触った行は
//!   その場で配列になる。
//! - 互換の読取は移行完了後も残す（ローリング更新中に旧バイナリが書いた文字列も正しく読む・安価）。
//!
//! # 冪等・チャンク・中断再開
//!
//! - テナント・テーブル単位。進捗（`data_link_value_migration`）を `FOR UPDATE` で押さえ、`record_int` 順に
//!   最大 N 件を処理して `cursor_record` を進める（1 チャンク = 1 Tx）。ワーカーが落ちても続きから再開する。
//! - 書き換えるのは**文字列の値を持つ行だけ**（何度流しても結果は同じ）。`rev` / `updated_at` は動かさない
//!   （利用者の変更ではない・意味は変わらない）。リビジョン履歴の差分は読取時に同じ形へ寄せる。
//! - レコード行は `FOR UPDATE SKIP LOCKED` で取る（書込を待たせない・デッドロックを作らない）。飛ばした行は
//!   書込が配列で書き直すか、末尾の検査で拾ってもう一周する。
//! - **parity**: 行ごとに、書き換え前後の値から導いた索引キー（`data_link` / `data_unique`）が一致することを
//!   確かめ、違えばチャンクをロールバックして失敗させる（索引は書き換えない＝作り直し不要であることの検証）。
//!   完了時に、そのテーブルのリンク列の索引を本体と突き合わせ（[`crate::index::parity`]）、結果を進捗に残す。
//! - 末尾まで進んだら全行を検査し、文字列が残っていなければ `data_table.link_value_format = 2` にして完了。

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde_json::{Map, Value};
use sqlx::types::Json;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::field_key::upgrade_link_value;
use crate::index::normalize::field_keys;
use crate::index::IndexTable;
use crate::key_migration::KEY_FORMAT_LEGACY;
use crate::model::{FieldDef, FieldType, TableKeys, TableSchema};
use crate::store::DataStore;
use crate::{map_db, DataError};

/// 1 チャンクで走査するレコード数。
pub const LINK_VALUE_CHUNK: i64 = 500;
/// 末尾の検査で文字列が残っていた場合にやり直す周回の上限（超えたら失敗として運用者に見せる）。
const MAX_PASSES: i32 = 5;
/// 全て配列の形式（移行済み・新規テーブル）。
const FORMAT_ARRAY: i16 = 2;

/// 1 チャンクの結果。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LinkValueStep {
    /// このチャンクで走査したレコード数（末尾の検査では 0）。
    pub scanned: u64,
    /// このチャンクで配列へ書き換えたレコード数。
    pub converted: u64,
    /// 移行が完了したか（`link_value_format = 2`）。
    pub done: bool,
}

/// テーブル単位の移行の状態（運用の観測用・値や内部キーは含めない）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkValueMigrationStatus {
    /// `running` / `done`。
    pub status: String,
    /// 書き換えたレコード数（全周回の合計）。
    pub rows_converted: i64,
    /// 末尾の検査で文字列が残っていて先頭からやり直した回数。
    pub passes: i32,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    /// 完了時の索引 parity 検証の時刻と不一致件数（リンク列の `data_link` / `data_unique`）。
    pub parity_checked_at: Option<DateTime<Utc>>,
    pub parity_mismatches: Option<i32>,
    pub last_error: Option<String>,
}

/// 移行の対象列（逆リンクは保存されないので対象外）。
fn link_fields(schema: &TableSchema) -> Vec<&FieldDef> {
    schema
        .fields
        .iter()
        .filter(|f| f.field_type == FieldType::RecordRef && !f.is_reverse_link())
        .collect()
}

/// 文字列の値を持つかの SQL 条件（`$3` = 対象列の JSONB キーの配列）。
const HAS_LEGACY_VALUE: &str =
    "EXISTS (SELECT 1 FROM unnest($3::text[]) k WHERE jsonb_typeof(r.data -> k) = 'string')";

impl DataStore {
    /// リンク値の移行を 1 チャンク進める（冪等・中断再開可）。
    pub async fn link_value_migration_step(
        &self,
        tenant_id: &str,
        table_id: Uuid,
        chunk: i64,
    ) -> Result<LinkValueStep, DataError> {
        let db = self.pools.for_tenant(tenant_id);
        let legacy_keys: Option<i16> = sqlx::query_scalar(
            "SELECT key_format FROM data_table WHERE tenant_id = $1 AND id = $2",
        )
        .bind(tenant_id)
        .bind(table_id)
        .fetch_optional(db)
        .await
        .map_err(map_db)?;
        match legacy_keys {
            None => return Err(DataError::NotFound),
            // JSONB キーが名前のままのテーブルは先に識別子を移行する（冪等）。
            Some(KEY_FORMAT_LEGACY) => {
                self.migrate_table_field_keys(tenant_id, table_id).await?;
            }
            Some(_) => {}
        }
        let mut tx = db.begin().await.map_err(map_db)?;
        // 書込と同じくテーブル行を FOR KEY SHARE（スキーマ改訂とは直列化・書込は並行できる）。
        let row: Option<(Json<TableSchema>, i32, i64, i16)> = sqlx::query_as(
            "SELECT schema, tenant_int, table_int, link_value_format FROM data_table \
             WHERE tenant_id = $1 AND id = $2 FOR KEY SHARE",
        )
        .bind(tenant_id)
        .bind(table_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db)?;
        let (schema, tenant_int, table_int, format) = row.ok_or(DataError::NotFound)?;
        if format == FORMAT_ARRAY {
            tx.rollback().await.map_err(map_db)?;
            return Ok(LinkValueStep {
                done: true,
                ..LinkValueStep::default()
            });
        }
        let keys = TableKeys {
            tenant_int,
            table_int,
        };
        let fields = link_fields(&schema.0);
        let json_keys: Vec<String> = fields.iter().map(|f| f.json_key()).collect();
        let (cursor, passes) = lock_progress(&mut tx, keys).await?;

        let ids: Vec<i64> = sqlx::query_scalar(
            "SELECT record_int FROM data_record WHERE tenant_int = $1 AND table_int = $2 \
               AND record_int > $3 ORDER BY record_int LIMIT $4",
        )
        .bind(tenant_int)
        .bind(table_int)
        .bind(cursor)
        .bind(chunk.max(1))
        .fetch_all(&mut *tx)
        .await
        .map_err(map_db)?;
        let Some(&hi) = ids.last().filter(|_| !fields.is_empty()) else {
            // 末尾（または対象列なし）: 残りを検査して完了するか、もう一周する。
            let done = self
                .finish_or_repeat(&mut tx, tenant_id, keys, &schema.0, &json_keys, passes)
                .await?;
            tx.commit().await.map_err(map_db)?;
            return Ok(LinkValueStep {
                done,
                ..LinkValueStep::default()
            });
        };

        let rows: Vec<(i64, Json<Value>)> = sqlx::query_as(&format!(
            "SELECT r.record_int, r.data FROM data_record r \
             WHERE r.tenant_int = $1 AND r.table_int = $2 AND {HAS_LEGACY_VALUE} \
               AND r.record_int > $4 AND r.record_int <= $5 \
             ORDER BY r.record_int FOR UPDATE SKIP LOCKED"
        ))
        .bind(tenant_int)
        .bind(table_int)
        .bind(&json_keys)
        .bind(cursor)
        .bind(hi)
        .fetch_all(&mut *tx)
        .await
        .map_err(map_db)?;
        let mut ints = Vec::with_capacity(rows.len());
        let mut bodies = Vec::with_capacity(rows.len());
        for (record_int, data) in rows {
            let Some(obj) = data.0.as_object() else {
                continue;
            };
            ints.push(record_int);
            bodies.push(Json(Value::Object(convert_checked(
                &fields,
                obj,
                self.max_multi_values,
            )?)));
        }
        if !ints.is_empty() {
            sqlx::query(
                "UPDATE data_record r SET data = u.data \
                 FROM unnest($3::int8[], $4::jsonb[]) AS u(record_int, data) \
                 WHERE r.tenant_int = $1 AND r.table_int = $2 AND r.record_int = u.record_int",
            )
            .bind(tenant_int)
            .bind(table_int)
            .bind(&ints)
            .bind(&bodies)
            .execute(&mut *tx)
            .await
            .map_err(map_db)?;
        }
        sqlx::query(
            "UPDATE data_link_value_migration SET cursor_record = $3, \
               rows_converted = rows_converted + $4, updated_at = now(), last_error = NULL \
             WHERE tenant_int = $1 AND table_int = $2",
        )
        .bind(tenant_int)
        .bind(table_int)
        .bind(hi)
        .bind(i64::try_from(ints.len()).unwrap_or(i64::MAX))
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        tx.commit().await.map_err(map_db)?;
        Ok(LinkValueStep {
            scanned: ids.len() as u64,
            converted: ints.len() as u64,
            done: false,
        })
    }

    /// 末尾まで進んだ: 文字列が残っていれば先頭からもう一周し、無ければ完了にする。完了なら `true`。
    async fn finish_or_repeat(
        &self,
        conn: &mut PgConnection,
        tenant_id: &str,
        keys: TableKeys,
        schema: &TableSchema,
        json_keys: &[String],
        passes: i32,
    ) -> Result<bool, DataError> {
        let remaining: bool = if json_keys.is_empty() {
            false
        } else {
            sqlx::query_scalar(&format!(
                "SELECT EXISTS (SELECT 1 FROM data_record r \
                 WHERE r.tenant_int = $1 AND r.table_int = $2 AND {HAS_LEGACY_VALUE})"
            ))
            .bind(keys.tenant_int)
            .bind(keys.table_int)
            .bind(json_keys)
            .fetch_one(&mut *conn)
            .await
            .map_err(map_db)?
        };
        if remaining {
            if passes + 1 >= MAX_PASSES {
                let msg = "末尾の検査で文字列のリンク値が残り続けています（旧バイナリの書込が続いている可能性）";
                sqlx::query(
                    "UPDATE data_link_value_migration SET last_error = $3, updated_at = now() \
                     WHERE tenant_int = $1 AND table_int = $2",
                )
                .bind(keys.tenant_int)
                .bind(keys.table_int)
                .bind(msg)
                .execute(&mut *conn)
                .await
                .map_err(map_db)?;
                // 進捗は残して失敗させる（jobq のバックオフ・DLQ で運用者に見える）。周回数は戻さない。
                return Err(DataError::Conflict(msg.into()));
            }
            sqlx::query(
                "UPDATE data_link_value_migration SET cursor_record = 0, passes = passes + 1, \
                   updated_at = now() WHERE tenant_int = $1 AND table_int = $2",
            )
            .bind(keys.tenant_int)
            .bind(keys.table_int)
            .execute(&mut *conn)
            .await
            .map_err(map_db)?;
            return Ok(false);
        }
        // 書き換えは索引キーを変えない（行ごとに検証済み）。完了時に本体と索引を突き合わせて記録する。
        let ids: BTreeSet<_> = link_fields(schema).iter().map(|f| f.id).collect();
        let report =
            crate::index::parity::verify_fields(&mut *conn, keys, schema, tenant_id, &ids).await?;
        let mismatches: i64 = report.mismatches.iter().map(|m| m.missing + m.extra).sum();
        if mismatches > 0 {
            tracing::error!(table_int = keys.table_int, mismatches = ?report.mismatches,
                "リンク値の移行の完了時に索引の parity 不一致（値の移行とは独立の既存の不一致）");
        }
        sqlx::query(
            "UPDATE data_table SET link_value_format = $3 \
             WHERE tenant_int = $1 AND table_int = $2",
        )
        .bind(keys.tenant_int)
        .bind(keys.table_int)
        .bind(FORMAT_ARRAY)
        .execute(&mut *conn)
        .await
        .map_err(map_db)?;
        sqlx::query(
            "UPDATE data_link_value_migration SET status = 'done', finished_at = now(), \
               updated_at = now(), parity_checked_at = now(), parity_mismatches = $3, \
               last_error = NULL \
             WHERE tenant_int = $1 AND table_int = $2",
        )
        .bind(keys.tenant_int)
        .bind(keys.table_int)
        .bind(i32::try_from(mismatches).unwrap_or(i32::MAX))
        .execute(&mut *conn)
        .await
        .map_err(map_db)?;
        Ok(true)
    }

    /// 1 テーブルの移行を最後まで流す（運用・テスト用。常駐ワーカーはジョブ単位で進める）。
    pub async fn migrate_table_link_values(
        &self,
        tenant_id: &str,
        table_id: Uuid,
    ) -> Result<u64, DataError> {
        let mut converted = 0;
        loop {
            let step = self
                .link_value_migration_step(tenant_id, table_id, LINK_VALUE_CHUNK)
                .await?;
            converted += step.converted;
            if step.done {
                return Ok(converted);
            }
        }
    }

    /// テーブルの移行の状態（未着手・新規テーブルは `None`）。
    pub async fn link_value_migration_status(
        &self,
        tenant_id: &str,
        table_id: Uuid,
    ) -> Result<Option<LinkValueMigrationStatus>, DataError> {
        let Some(keys) = self.table_keys_of(tenant_id, table_id).await? else {
            return Err(DataError::NotFound);
        };
        type Row = (
            String,
            i64,
            i32,
            DateTime<Utc>,
            Option<DateTime<Utc>>,
            Option<DateTime<Utc>>,
            Option<i32>,
            Option<String>,
        );
        let row: Option<Row> = sqlx::query_as(
            "SELECT status, rows_converted, passes, started_at, finished_at, parity_checked_at, \
                    parity_mismatches, last_error \
             FROM data_link_value_migration WHERE tenant_int = $1 AND table_int = $2",
        )
        .bind(keys.tenant_int)
        .bind(keys.table_int)
        .fetch_optional(self.pools.for_tenant(tenant_id))
        .await
        .map_err(map_db)?;
        Ok(row.map(
            |(
                status,
                rows_converted,
                passes,
                started_at,
                finished_at,
                parity_checked_at,
                parity_mismatches,
                last_error,
            )| LinkValueMigrationStatus {
                status,
                rows_converted,
                passes,
                started_at,
                finished_at,
                parity_checked_at,
                parity_mismatches,
                last_error,
            },
        ))
    }

    /// 未移行（文字列が残り得る）の生存テーブル数（全プール合算・進捗の観測用）。
    pub async fn link_value_migration_remaining(&self) -> Result<i64, DataError> {
        let mut total = 0;
        for (_, pool) in self.pools.all() {
            // shard-safety: allow(cross-tenant) 進捗の観測（プールごとに数えて合算する）。
            let n: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM data_table \
                 WHERE link_value_format = 1 AND deleted_at IS NULL",
            )
            .fetch_one(pool)
            .await
            .map_err(map_db)?;
            total += n;
        }
        Ok(total)
    }

    /// 未着手の生存テーブル（識別子の移行済み）に進捗行を作り、移行ジョブを積む（索引スイープから呼ぶ）。
    /// 積んだ数を返す。進捗行の作成と投入は同じ Tx（二重投入しない・取りこぼさない）。
    pub(crate) async fn enqueue_link_value_migrations(&self, limit: i64) -> Result<u64, DataError> {
        let mut queued = 0;
        for (_, pool) in self.pools.all() {
            // shard-safety: allow(cross-tenant) 保守スイープ。プールごとに問い合わせる（合算は Rust 側）。
            let targets: Vec<(String, Uuid, i32, i64)> = sqlx::query_as(
                "SELECT t.tenant_id, t.id, t.tenant_int, t.table_int FROM data_table t \
                 WHERE t.link_value_format = 1 AND t.key_format = 2 AND t.deleted_at IS NULL \
                   AND NOT EXISTS (SELECT 1 FROM data_link_value_migration m \
                                   WHERE m.tenant_int = t.tenant_int AND m.table_int = t.table_int) \
                 ORDER BY t.tenant_id, t.id LIMIT $1",
            )
            .bind(limit)
            .fetch_all(pool)
            .await
            .map_err(map_db)?;
            for (tenant_id, id, tenant_int, table_int) in targets {
                let mut tx = pool.begin().await.map_err(map_db)?;
                let keys = TableKeys {
                    tenant_int,
                    table_int,
                };
                if insert_progress(&mut tx, keys).await? {
                    crate::index::backfill::enqueue_op(&mut tx, LINK_VALUES_OP, &tenant_id, id)
                        .await?;
                    queued += 1;
                }
                tx.commit().await.map_err(map_db)?;
            }
        }
        Ok(queued)
    }
}

/// 1 行の文字列の値を配列へ書き換え、書き換え前後で索引キーが変わらないことを確かめる。
fn convert_checked(
    fields: &[&FieldDef],
    obj: &Map<String, Value>,
    max_multi_values: usize,
) -> Result<Map<String, Value>, DataError> {
    let mut out = obj.clone();
    for f in fields {
        let jk = f.json_key();
        let Some(old @ Value::String(_)) = obj.get(&jk) else {
            continue;
        };
        let new = upgrade_link_value(old);
        let mut tables = vec![IndexTable::Link];
        if f.unique {
            tables.push(IndexTable::Unique);
        }
        for t in tables {
            let before = field_keys(f, t, old, max_multi_values)?;
            let after = field_keys(f, t, &new, max_multi_values)?;
            if before != after {
                return Err(DataError::Internal(format!(
                    "リンク値の移行で '{}' の索引キーが変わります（移行を中止）",
                    f.key
                )));
            }
        }
        out.insert(jk, new);
    }
    Ok(out)
}

/// 索引系ジョブキューでの op 名。
pub(crate) const LINK_VALUES_OP: &str = "link_values";

/// 進捗行を作る（あれば何もしない）。作ったら `true`。
async fn insert_progress(conn: &mut PgConnection, keys: TableKeys) -> Result<bool, DataError> {
    let created = sqlx::query(
        "INSERT INTO data_link_value_migration (tenant_int, table_int, high_water) \
         SELECT $1, $2, coalesce(max(record_int), 0) FROM data_record \
         WHERE tenant_int = $1 AND table_int = $2 \
         ON CONFLICT (tenant_int, table_int) DO NOTHING",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .execute(&mut *conn)
    .await
    .map_err(map_db)?
    .rows_affected();
    Ok(created > 0)
}

/// 進捗行を（無ければ作って）`FOR UPDATE` で押さえ、`(cursor_record, passes)` を返す。
/// 同じテーブルのチャンクが並行しても直列化される（多重配信されたジョブ）。
async fn lock_progress(conn: &mut PgConnection, keys: TableKeys) -> Result<(i64, i32), DataError> {
    insert_progress(&mut *conn, keys).await?;
    sqlx::query_as(
        "SELECT cursor_record, passes FROM data_link_value_migration \
         WHERE tenant_int = $1 AND table_int = $2 FOR UPDATE",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    fn link(name: &str, id: u16) -> FieldDef {
        let mut f = FieldDef::new(name, FieldType::RecordRef);
        f.id = id;
        f.ref_table = Some(Uuid::nil());
        f
    }

    #[test]
    fn converts_only_legacy_strings_and_keeps_index_keys() {
        let store_fields = [link("customer", 16), {
            let mut u = link("owner_ref", 17);
            u.unique = true;
            u
        }];
        let fields: Vec<&FieldDef> = store_fields.iter().collect();
        let obj = json!({
            "f16": "6F1C9A3E-0000-4000-8000-000000000001",
            "f17": ["6f1c9a3e-0000-4000-8000-000000000002"],
            "f18": "untouched"
        });
        let out = convert_checked(&fields, obj.as_object().unwrap(), 100).unwrap();
        assert_eq!(
            Value::Object(out),
            json!({
                "f16": ["6f1c9a3e-0000-4000-8000-000000000001"],
                "f17": ["6f1c9a3e-0000-4000-8000-000000000002"],
                "f18": "untouched"
            })
        );
    }
}
