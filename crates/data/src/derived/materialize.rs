//! 派生値の材料化（data-platform §7.4・PIT-71）。
//!
//! 材料化するのは `invariant`（全閲覧者・全時刻で同値と証明できた）かつ `indexed` の列だけ。値は
//! 索引テーブル（num / text）の行としてだけ持ち、フィルタ・ソートに使う（表示は常に読取時計算）。
//!
//! | 契機 | 処理（同一 Tx） |
//! |---|---|
//! | 参照元レコードの作成・更新 | そのレコードの材料化値を計算して索引行を書き直す（参照先の行は `FOR SHARE`） |
//! | 参照先レコードの更新・削除 | 依存登録から影響する参照元レコードを引き、**再計算待ち**に積んでジョブを enqueue。待ちがある列はクエリ対象外（409） |
//! | 参照先に `row_policy` / `field_policy` を追加・個別共有の発生・テーブル削除 | 依存する列を `per_viewer` へ**降格**し、索引行・再計算待ち・依存登録を破棄 |
//! | テーブル ReBAC の変化（包含が崩れた） | 読取・クエリの入口で 4 階層を検査し直し、崩れていれば同じく降格 |
//!
//! 参照先の行を `FOR SHARE` で読むので、参照先の更新（`FOR UPDATE`）とは直列化される。先に更新が
//! 行ロックを取れば、こちらは更新後の値を読む。先にこちらが読めば、更新側の依存走査はこちらのコミット後に
//! 走り、新しいリンクを見て再計算待ちを積む（取りこぼさない）。

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use authz::{AuthContext, Relation, Subject};
use serde_json::{json, Map, Value};
use sqlx::types::Json;
use sqlx::PgConnection;
use uuid::Uuid;

use super::compute::{linked_ids, RefRows, RowEval};
use super::graph::{Catalog, CatalogTable};
use super::types::Materialization;
use crate::field_key::decode_record;
use crate::formula::decimal::Decimal;
use crate::index::normalize::text_key;
use crate::index::state::{derived_table, field_id_i16, sync_ledger};
use crate::index::IndexTable;
use crate::model::{DataTable, FieldId, TableKeys, TableSchema};
use crate::store::{DataStore, TableRow, TABLE_COLS};
use crate::{map_db, DataError, DEFAULT_AGGREGATE_MIN_ROWS};

/// 再計算ジョブの 1 チャンクの件数。
pub(crate) const RECOMPUTE_CHUNK: i64 = 200;

/// 材料化する列があるか。
pub(crate) fn has_materialized(schema: &TableSchema) -> bool {
    schema
        .fields
        .iter()
        .any(crate::model::FieldDef::is_materialized)
}

/// 材料化する列が辿るリンク列の参照名（参照先の行を引く対象）。
fn invariant_links(schema: &TableSchema) -> BTreeMap<String, Uuid> {
    let mut out = BTreeMap::new();
    for f in &schema.fields {
        if f.materialization != Some(Materialization::Invariant) {
            continue;
        }
        let mut add = |link: &str| {
            if let Some(rt) = schema.field(link).and_then(|l| l.ref_table) {
                out.insert(link.to_string(), rt);
            }
        };
        if let Some(d) = &f.lookup {
            add(&d.via_field);
        }
        if let Some(d) = &f.rollup {
            add(&d.link);
        }
        if let Some(e) = &f.formula {
            for n in e.rollups() {
                if let crate::formula::FormulaExpr::Rollup { link, .. } = n {
                    add(link);
                }
            }
        }
    }
    out
}

/// 生存テーブル行を Tx 上で引く（保守経路・tenant スコープ）。
async fn table_row(
    conn: &mut PgConnection,
    tenant_id: &str,
    id: Uuid,
    lock: &str,
) -> Result<Option<TableRow>, DataError> {
    sqlx::query_as(&format!(
        "SELECT {TABLE_COLS} FROM data_table \
         WHERE tenant_id = $1 AND id = $2 AND deleted_at IS NULL {lock}"
    ))
    .bind(tenant_id)
    .bind(id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db)
}

/// `invariant` の列を全行から計算する（システム計算・材料化専用。`invariant` は全閲覧者で同値と
/// 証明済みなので、閲覧者の権限を挟まずに引いた値が誰にとっても正しい）。
async fn compute_invariant(
    conn: &mut PgConnection,
    tenant_id: &str,
    table: &DataTable,
    rows: &[Map<String, Value>],
) -> Result<Vec<Map<String, Value>>, DataError> {
    let mut refs: HashMap<String, RefRows> = HashMap::new();
    for (link, rt) in invariant_links(&table.schema) {
        let ids: Vec<Uuid> = rows
            .iter()
            .flat_map(|r| linked_ids(r, &link))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let Some(rrow) = table_row(conn, tenant_id, rt, "").await? else {
            refs.insert(link, RefRows::default());
            continue;
        };
        let rtab = rrow.into_table();
        let fetched: Vec<(Uuid, Json<Value>)> = if ids.is_empty() {
            Vec::new()
        } else {
            sqlx::query_as(
                "SELECT id, data FROM data_record \
                 WHERE tenant_int = $1 AND table_int = $2 AND id = ANY($3::uuid[]) \
                 ORDER BY record_int FOR SHARE",
            )
            .bind(rtab.keys.tenant_int)
            .bind(rtab.keys.table_int)
            .bind(&ids)
            .fetch_all(&mut *conn)
            .await
            .map_err(map_db)?
        };
        refs.insert(
            link,
            RefRows {
                rows: fetched
                    .into_iter()
                    .map(|(id, d)| (id, decode_record(&rtab.schema, &d.0)))
                    .collect(),
                types: rtab
                    .schema
                    .fields
                    .iter()
                    .map(|f| (f.key.clone(), f.field_type))
                    .collect(),
                min_rows: rtab
                    .schema
                    .aggregate_min_rows
                    .unwrap_or(DEFAULT_AGGREGATE_MIN_ROWS),
            },
        );
    }
    let eval = RowEval {
        schema: &table.schema,
        refs: &refs,
        now: chrono::Utc::now(),
        only_invariant: true,
        masked: &HashSet::new(),
    };
    Ok(rows.iter().map(|r| eval.evaluate(r)).collect())
}

/// 材料化した値の索引行を書き直す（1 レコード × 材料化列）。
async fn write_index_rows(
    conn: &mut PgConnection,
    table: &DataTable,
    record_int: i64,
    values: &Map<String, Value>,
) -> Result<(), DataError> {
    for f in table.schema.fields.iter().filter(|f| f.is_materialized()) {
        let fid = field_id_i16(f.id)?;
        let t = derived_table(f);
        sqlx::query(&format!(
            "DELETE FROM {} WHERE tenant_int = $1 AND table_int = $2 AND field_id = $3 \
               AND record_int = $4",
            t.relation()
        ))
        .bind(table.keys.tenant_int)
        .bind(table.keys.table_int)
        .bind(fid)
        .bind(record_int)
        .execute(&mut *conn)
        .await
        .map_err(map_db)?;
        let v = values.get(&f.key).unwrap_or(&Value::Null);
        match (t, v) {
            (_, Value::Null) => {}
            (IndexTable::Num, v) => {
                let d = match v {
                    Value::Number(n) => Decimal::from_number(n),
                    Value::String(s) => Decimal::parse(s),
                    _ => None,
                };
                let Some(d) = d else { continue };
                sqlx::query(
                    "INSERT INTO data_index_num (tenant_int, table_int, field_id, record_int, ord, val) \
                     VALUES ($1, $2, $3, $4, 0, $5::numeric)",
                )
                .bind(table.keys.tenant_int)
                .bind(table.keys.table_int)
                .bind(fid)
                .bind(record_int)
                .bind(d.canonical())
                .execute(&mut *conn)
                .await
                .map_err(map_db)?;
            }
            (_, v) => {
                let s = match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                let k = text_key(0, &s);
                sqlx::query(
                    "INSERT INTO data_index_text \
                       (tenant_int, table_int, field_id, record_int, ord, val, truncated) \
                     VALUES ($1, $2, $3, $4, 0, $5, $6)",
                )
                .bind(table.keys.tenant_int)
                .bind(table.keys.table_int)
                .bind(fid)
                .bind(record_int)
                .bind(&k.val)
                .bind(k.truncated)
                .execute(&mut *conn)
                .await
                .map_err(map_db)?;
            }
        }
    }
    Ok(())
}

/// 再計算待ちを積み、ジョブを enqueue する（参照先の変更 Tx・昇格時）。
async fn mark_pending(
    conn: &mut PgConnection,
    keys: TableKeys,
    tenant_id: &str,
    table_id: Uuid,
    fields: &[i16],
    record_ints: &[i64],
) -> Result<(), DataError> {
    if fields.is_empty() || record_ints.is_empty() {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO data_derived_pending (tenant_int, table_int, field_id, record_int) \
         SELECT $1, $2, f, r FROM unnest($3::int2[]) AS f CROSS JOIN unnest($4::int8[]) AS r \
         ON CONFLICT (tenant_int, table_int, field_id, record_int) \
         DO UPDATE SET gen = data_derived_pending.gen + 1, updated_at = now()",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .bind(fields)
    .bind(record_ints)
    .execute(&mut *conn)
    .await
    .map_err(map_db)?;
    crate::index::backfill::enqueue_op(conn, "derived", tenant_id, table_id).await
}

/// 表全体を再計算待ちにする（材料化列が増えた・昇格したとき）。
pub(crate) async fn mark_all_pending(
    conn: &mut PgConnection,
    keys: TableKeys,
    tenant_id: &str,
    table_id: Uuid,
    fields: &[FieldId],
) -> Result<(), DataError> {
    let fids: Vec<i16> = fields
        .iter()
        .map(|f| field_id_i16(*f))
        .collect::<Result<_, _>>()?;
    if fids.is_empty() {
        return Ok(());
    }
    let n = sqlx::query(
        "INSERT INTO data_derived_pending (tenant_int, table_int, field_id, record_int) \
         SELECT r.tenant_int, r.table_int, f, r.record_int \
         FROM data_record r CROSS JOIN unnest($3::int2[]) AS f \
         WHERE r.tenant_int = $1 AND r.table_int = $2 \
         ON CONFLICT (tenant_int, table_int, field_id, record_int) \
         DO UPDATE SET gen = data_derived_pending.gen + 1, updated_at = now()",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .bind(&fids)
    .execute(&mut *conn)
    .await
    .map_err(map_db)?
    .rows_affected();
    if n > 0 {
        crate::index::backfill::enqueue_op(conn, "derived", tenant_id, table_id).await?;
    }
    Ok(())
}

/// 材料化した列の依存登録の 1 行。
#[derive(Debug, Clone, Copy)]
pub(crate) struct Dep {
    /// 材料化した列。
    pub field: FieldId,
    /// 参照先テーブルの内部キー。
    pub ref_table_int: i64,
    /// 辿るリンク列（参照元側）。
    pub link: FieldId,
    /// リンクの対称列（参照先側）。あれば影響する参照元を逆方向行から主キーで引ける。
    pub sym: Option<FieldId>,
}

/// 依存登録を宣言へ合わせる（スキーマ保存の Tx）。
pub(crate) async fn sync_deps(
    conn: &mut PgConnection,
    keys: TableKeys,
    deps: &[Dep],
) -> Result<(), DataError> {
    sqlx::query("DELETE FROM data_derived_dep WHERE tenant_int = $1 AND table_int = $2")
        .bind(keys.tenant_int)
        .bind(keys.table_int)
        .execute(&mut *conn)
        .await
        .map_err(map_db)?;
    for d in deps {
        sqlx::query(
            "INSERT INTO data_derived_dep \
               (tenant_int, ref_table_int, table_int, field_id, link_field_id, sym_field_id) \
             VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT DO NOTHING",
        )
        .bind(keys.tenant_int)
        .bind(d.ref_table_int)
        .bind(keys.table_int)
        .bind(field_id_i16(d.field)?)
        .bind(field_id_i16(d.link)?)
        .bind(d.sym.map(field_id_i16).transpose()?)
        .execute(&mut *conn)
        .await
        .map_err(map_db)?;
    }
    Ok(())
}

/// 再計算待ちを消す（降格・レコード削除）。`fields = None` はレコードの全列。
async fn clear_pending(
    conn: &mut PgConnection,
    keys: TableKeys,
    fields: Option<&[i16]>,
    record: Option<i64>,
) -> Result<(), DataError> {
    sqlx::query(
        "DELETE FROM data_derived_pending WHERE tenant_int = $1 AND table_int = $2 \
           AND ($3::int2[] IS NULL OR field_id = ANY($3::int2[])) \
           AND ($4::int8 IS NULL OR record_int = $4)",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .bind(fields.map(<[i16]>::to_vec))
    .bind(record)
    .execute(&mut *conn)
    .await
    .map_err(map_db)?;
    Ok(())
}

/// 列に再計算待ちがあるか（あればクエリ対象外）。
pub(crate) async fn pending_fields(
    conn: &mut PgConnection,
    keys: TableKeys,
    fields: &[i16],
) -> Result<Vec<i16>, DataError> {
    if fields.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_scalar(
        "SELECT f FROM unnest($3::int2[]) AS f WHERE EXISTS ( \
           SELECT 1 FROM data_derived_pending p \
           WHERE p.tenant_int = $1 AND p.table_int = $2 AND p.field_id = f)",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .bind(fields)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db)
}

/// 参照先テーブル（`ref_keys`）のレコードが変わった: 依存する参照元レコードを再計算待ちに積む。
async fn enqueue_dependents(
    conn: &mut PgConnection,
    tenant_id: &str,
    ref_keys: TableKeys,
    record_ints: &[i64],
) -> Result<(), DataError> {
    let deps: Vec<(i64, i16, i16, Option<i16>)> = sqlx::query_as(
        "SELECT table_int, field_id, link_field_id, sym_field_id FROM data_derived_dep \
         WHERE tenant_int = $1 AND ref_table_int = $2",
    )
    .bind(ref_keys.tenant_int)
    .bind(ref_keys.table_int)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db)?;
    if deps.is_empty() {
        return Ok(());
    }
    // (参照元 table_int, リンク列, 対称列) → 材料化した列。
    type LinkKey = (i64, i16, Option<i16>);
    let mut by_link: BTreeMap<LinkKey, Vec<i16>> = BTreeMap::new();
    for (t, f, l, sym) in deps {
        by_link.entry((t, l, sym)).or_default().push(f);
    }
    for ((table_int, link, sym), fields) in by_link {
        let affected: Vec<i64> = match sym {
            // 対称列あり: 参照先の逆方向行（参照先のパーティション・主キーの前方一致）から引く。
            Some(sym) => sqlx::query_scalar(
                "SELECT DISTINCT dst_record FROM data_link \
                 WHERE tenant_int = $1 AND table_int = $2 AND field_id = $3 \
                   AND src_record = ANY($4::int8[]) AND dst_table_int = $5",
            )
            .bind(ref_keys.tenant_int)
            .bind(ref_keys.table_int)
            .bind(sym)
            .bind(record_ints)
            .bind(table_int)
            .fetch_all(&mut *conn)
            .await
            .map_err(map_db)?,
            // 片方向: 参照元のパーティション（table_int）の中で、変わった参照先を指す辺だけを引く。
            None => sqlx::query_scalar(
                "SELECT DISTINCT src_record FROM data_link \
                 WHERE tenant_int = $1 AND table_int = $2 AND field_id = $3 \
                   AND dst_table_int = $4 AND dst_record = ANY($5::int8[])",
            )
            .bind(ref_keys.tenant_int)
            .bind(table_int)
            .bind(link)
            .bind(ref_keys.table_int)
            .bind(record_ints)
            .fetch_all(&mut *conn)
            .await
            .map_err(map_db)?,
        };
        if affected.is_empty() {
            continue;
        }
        let table_id: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM data_table WHERE tenant_id = $1 AND table_int = $2")
                .bind(tenant_id)
                .bind(table_int)
                .fetch_optional(&mut *conn)
                .await
                .map_err(map_db)?;
        let Some(table_id) = table_id else { continue };
        let keys = TableKeys {
            tenant_int: ref_keys.tenant_int,
            table_int,
        };
        mark_pending(conn, keys, tenant_id, table_id, &fields, &affected).await?;
    }
    Ok(())
}

/// レコード書込の同一 Tx で呼ぶ: 自分の材料化値を計算し、依存する参照元を再計算待ちに積む。
///
/// `data` は書込後の外部形の値（削除なら `None`）。
pub(crate) async fn after_record_write(
    conn: &mut PgConnection,
    tenant_id: &str,
    table: &DataTable,
    record_int: i64,
    data: Option<&Map<String, Value>>,
) -> Result<(), DataError> {
    match data {
        Some(d) if has_materialized(&table.schema) => {
            let values = compute_invariant(conn, tenant_id, table, std::slice::from_ref(d)).await?;
            if let Some(v) = values.first() {
                write_index_rows(conn, table, record_int, v).await?;
            }
            clear_pending(conn, table.keys, None, Some(record_int)).await?;
        }
        Some(_) => {}
        None => {
            clear_pending(conn, table.keys, None, Some(record_int)).await?;
            // 依存する参照元を（逆方向行が残っているうちに）再計算待ちへ積んでから、
            // 対称列の逆方向行（このレコードが参照先側）を消す。参照元の順方向行は参照元の本体と
            // 揃えて残す（宙に浮いた参照は読取で落ちる）。
            enqueue_dependents(conn, tenant_id, table.keys, &[record_int]).await?;
            let rev: Vec<i16> = table
                .schema
                .fields
                .iter()
                .filter(|f| f.is_reverse_link())
                .map(|f| field_id_i16(f.id))
                .collect::<Result<_, _>>()?;
            return crate::index::link::delete_reverse_rows(conn, table.keys, &rev, record_int)
                .await;
        }
    }
    enqueue_dependents(conn, tenant_id, table.keys, &[record_int]).await
}

/// 材料化した列を `per_viewer` へ降格する（**索引行・再計算待ち・依存登録を同一 Tx で破棄**・PIT-71）。
///
/// 対象テーブルの行を `FOR UPDATE` で押さえ、スキーマの宣言（`materialization` / `indexed`）を書き換えて
/// 版を進め、台帳を宣言へ合わせる（索引行は台帳の同期が同じ Tx で消す）。
pub(crate) async fn demote(
    conn: &mut PgConnection,
    tenant_id: &str,
    table_id: Uuid,
    fields: &[FieldId],
    reason: &str,
) -> Result<bool, DataError> {
    let Some(row) = table_row(conn, tenant_id, table_id, "FOR UPDATE").await? else {
        return Ok(false);
    };
    let keys = TableKeys {
        tenant_int: row.tenant_int,
        table_int: row.table_int,
    };
    let mut schema = row.schema.0.clone();
    let mut changed = Vec::new();
    for f in &mut schema.fields {
        if fields.contains(&f.id) && f.is_materialized() {
            f.materialization = Some(Materialization::PerViewer);
            f.indexed = false;
            changed.push(f.id);
        }
    }
    if changed.is_empty() {
        return Ok(false);
    }
    sqlx::query(
        "UPDATE data_table SET schema = $3, schema_version = schema_version + 1, updated_at = now() \
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant_id)
    .bind(table_id)
    .bind(Json(&schema))
    .execute(&mut *conn)
    .await
    .map_err(map_db)?;
    // 宣言から外れた (列, 索引表) は台帳が absent にし、索引行を同じ Tx で消す。
    sync_ledger(conn, keys, &schema, false).await?;
    let fids: Vec<i16> = changed
        .iter()
        .map(|f| field_id_i16(*f))
        .collect::<Result<_, _>>()?;
    clear_pending(conn, keys, Some(&fids), None).await?;
    sqlx::query(
        "DELETE FROM data_derived_dep WHERE tenant_int = $1 AND table_int = $2 \
           AND field_id = ANY($3::int2[])",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .bind(&fids)
    .execute(&mut *conn)
    .await
    .map_err(map_db)?;
    tracing::warn!(
        target: "security",
        alert = "data.derived.demoted",
        %table_id,
        fields = ?changed,
        reason,
        "材料化した派生列を per_viewer へ降格し、材料化データを破棄しました（PIT-71）"
    );
    Ok(true)
}

/// 参照先 `ref_keys` の前提が変わった（スキーマ改訂・個別共有・削除）: 依存する列を検査し直して降格する。
///
/// `ref_table` が `None` なら参照先は消えた（全て降格）。①テーブル ReBAC の包含はここでは変わらない
/// 前提で②③④だけを見る（ReBAC の変化は読取・クエリの入口の検査が拾う）。
pub(crate) async fn demote_dependents(
    conn: &mut PgConnection,
    tenant_id: &str,
    ref_keys: TableKeys,
    ref_table: Option<(Uuid, &CatalogTable)>,
    reason: &str,
) -> Result<(), DataError> {
    let deps: Vec<(i64, i16)> = sqlx::query_as(
        "SELECT DISTINCT table_int, field_id FROM data_derived_dep \
         WHERE tenant_int = $1 AND ref_table_int = $2 ORDER BY table_int, field_id",
    )
    .bind(ref_keys.tenant_int)
    .bind(ref_keys.table_int)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db)?;
    let mut by_table: BTreeMap<i64, Vec<FieldId>> = BTreeMap::new();
    for (t, f) in deps {
        by_table
            .entry(t)
            .or_default()
            .push(FieldId::try_from(f).unwrap_or(0));
    }
    for (table_int, fields) in by_table {
        let src: Option<(Uuid, Json<TableSchema>, Option<Uuid>)> = sqlx::query_as(
            "SELECT id, schema, app_id FROM data_table \
             WHERE tenant_id = $1 AND table_int = $2 AND deleted_at IS NULL",
        )
        .bind(tenant_id)
        .bind(table_int)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db)?;
        let Some((src_id, Json(src_schema), app_id)) = src else {
            continue;
        };
        let mut cat = Catalog::default();
        cat.tables.insert(
            src_id,
            CatalogTable {
                schema: src_schema.clone(),
                app_id,
                has_record_shares: false,
                keys: TableKeys {
                    tenant_int: ref_keys.tenant_int,
                    table_int,
                },
            },
        );
        let mut contained = HashSet::new();
        if let Some((rid, rt)) = ref_table {
            cat.tables.insert(rid, rt.clone());
            contained.insert(rid);
        }
        let broken: Vec<FieldId> = fields
            .into_iter()
            .filter(|f| {
                src_schema.field_by_id(*f).is_some_and(|d| {
                    cat.materialization(src_id, &d.key, &contained) != Materialization::Invariant
                })
            })
            .collect();
        if !broken.is_empty() {
            demote(conn, tenant_id, src_id, &broken, reason).await?;
        }
    }
    Ok(())
}

/// スキーマ・アプリ・共有フラグからカタログの 1 行を作る。
pub(crate) fn catalog_entry(t: &DataTable) -> CatalogTable {
    CatalogTable {
        schema: t.schema.clone(),
        app_id: t.app_id,
        has_record_shares: t.has_record_shares,
        keys: t.keys,
    }
}

/// OpenFGA の直接タプルの比較で「`dst` の viewer 集合が `src` の viewer 集合を包含する」と証明できるか。
///
/// `data_table` の viewer は直接タプル（owner / editor / viewer）だけから決まる（親からの継承が無い）ので、
/// `src` の各タプルの主体が `dst` に owner / editor / viewer のいずれかで載っていれば包含が成り立つ
/// （健全だが完全ではない＝証明できなければ `per_viewer`）。`src_tuples` が `None` なら `src` を読む。
pub(crate) async fn viewers_contained(
    authz: &dyn authz::AuthzClient,
    ctx: &AuthContext,
    src: Option<Uuid>,
    predicted_src_owner: Option<&Subject>,
    dst: Uuid,
) -> Result<bool, DataError> {
    let rels = [Relation::Owner, Relation::Editor, Relation::Viewer];
    let read = |id: Uuid| {
        let obj = ctx.ns().data_table(&id.to_string());
        async move {
            authz
                .read_tuples(&obj, None)
                .await
                .map_err(|e| DataError::Internal(format!("authz: {e}")))
        }
    };
    let dst_users: HashSet<String> = read(dst)
        .await?
        .into_iter()
        .filter(|t| rels.iter().any(|r| r.as_str() == t.relation))
        .map(|t| t.user)
        .collect();
    let src_users: Vec<String> = match (src, predicted_src_owner) {
        (Some(id), _) => read(id)
            .await?
            .into_iter()
            .filter(|t| rels.iter().any(|r| r.as_str() == t.relation))
            .map(|t| t.user)
            .collect(),
        (None, Some(owner)) => vec![owner.to_string()],
        (None, None) => return Ok(false),
    };
    Ok(!src_users.is_empty() && src_users.iter().all(|u| dst_users.contains(u)))
}

impl DataStore {
    /// 材料化した列の前提（4 階層）を今の状態で検査し直し、崩れた列を降格する。降格したら `true`。
    ///
    /// 認可材料（OpenFGA のタプル・参照先のスキーマと共有フラグ）は毎回引く（PIT-18）。
    pub(crate) async fn verify_materialized(
        &self,
        ctx: &AuthContext,
        table: &DataTable,
    ) -> Result<bool, DataError> {
        if !has_materialized(&table.schema) {
            return Ok(false);
        }
        let mut cat = Catalog::default();
        cat.tables.insert(table.id, catalog_entry(table));
        let mut contained = HashSet::new();
        for rt in invariant_links(&table.schema).into_values() {
            if cat.tables.contains_key(&rt) {
                continue;
            }
            let r = match self.fetch_live(ctx, rt).await {
                Ok(t) => t,
                Err(DataError::NotFound) => continue,
                Err(e) => return Err(e),
            };
            if viewers_contained(self.authz.as_ref(), ctx, Some(table.id), None, rt).await? {
                contained.insert(rt);
            }
            cat.tables.insert(rt, catalog_entry(&r));
        }
        let broken: Vec<FieldId> = table
            .schema
            .fields
            .iter()
            .filter(|f| f.is_materialized())
            .filter(|f| {
                cat.materialization(table.id, &f.key, &contained) != Materialization::Invariant
            })
            .map(|f| f.id)
            .collect();
        if broken.is_empty() {
            return Ok(false);
        }
        let mut tx = self
            .pools
            .for_tenant(&ctx.tenant_id)
            .begin()
            .await
            .map_err(map_db)?;
        let demoted = demote(
            &mut tx,
            &ctx.tenant_id,
            table.id,
            &broken,
            "参照先のテーブル ReBAC・行述語・マスク・個別共有のいずれかが変わった",
        )
        .await?;
        tx.commit().await.map_err(map_db)?;
        if demoted {
            self.record_audit_best_effort(
                ctx,
                "data.derived.demote",
                &table.id.to_string(),
                None,
                json!({ "fields": broken }),
            )
            .await;
        }
        Ok(demoted)
    }

    /// 材料化の前提を検査し直す（運用・ReBAC を変えた経路から呼ぶ入口）。降格したら `true`。
    ///
    /// テーブル ReBAC（タプル）の付与・剥奪は本 crate の外（インストール・共有の管理）でも起きるため、
    /// 変えた側はこれを呼んで材料化データを即時に破棄できる（呼ばなくても、次の読取・クエリの入口で
    /// 同じ検査が走る）。
    pub async fn reverify_materialization(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
    ) -> Result<bool, DataError> {
        let table = self.fetch_live(ctx, table_id).await?;
        self.verify_materialized(ctx, &table).await
    }

    /// 再計算ジョブの 1 チャンク（再計算待ちを `record_int` 順に処理する）。待ちが尽きたら `true`。
    pub async fn derived_recompute_step(
        &self,
        tenant_id: &str,
        table_id: Uuid,
        limit: i64,
    ) -> Result<bool, DataError> {
        let mut tx = self
            .pools
            .for_tenant(tenant_id)
            .begin()
            .await
            .map_err(map_db)?;
        let Some(row) = table_row(&mut tx, tenant_id, table_id, "").await? else {
            return Ok(true);
        };
        let table = row.into_table();
        // 書込と同じ順にテーブル行を FOR KEY SHARE（スキーマ改訂・降格と直列化）。
        crate::index::state::lock_plan(
            &mut tx,
            tenant_id,
            table_id,
            table.keys,
            table.schema_version,
        )
        .await?;
        // 待ちの行はロックしない（参照先の更新 Tx は参照先の行ロックを持ったまま待ちを積むので、ここで
        // 待ちの行を押さえてから参照先を FOR SHARE で待つとデッドロックする）。読んだ世代だけを最後に消す。
        let pending: Vec<(i16, i64, i64)> = sqlx::query_as(
            "SELECT field_id, record_int, gen FROM data_derived_pending \
             WHERE tenant_int = $1 AND table_int = $2 \
             ORDER BY record_int, field_id LIMIT $3",
        )
        .bind(table.keys.tenant_int)
        .bind(table.keys.table_int)
        .bind(limit)
        .fetch_all(&mut *tx)
        .await
        .map_err(map_db)?;
        if pending.is_empty() {
            tx.commit().await.map_err(map_db)?;
            return Ok(true);
        }
        let ints: Vec<i64> = pending
            .iter()
            .map(|p| p.1)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if has_materialized(&table.schema) {
            let recs: Vec<(i64, Json<Value>)> = sqlx::query_as(
                "SELECT record_int, data FROM data_record \
                 WHERE tenant_int = $1 AND table_int = $2 AND record_int = ANY($3::int8[]) \
                 ORDER BY record_int FOR SHARE",
            )
            .bind(table.keys.tenant_int)
            .bind(table.keys.table_int)
            .bind(&ints)
            .fetch_all(&mut *tx)
            .await
            .map_err(map_db)?;
            let data: Vec<Map<String, Value>> = recs
                .iter()
                .map(|(_, d)| decode_record(&table.schema, &d.0))
                .collect();
            let values = compute_invariant(&mut tx, tenant_id, &table, &data).await?;
            for ((rint, _), v) in recs.iter().zip(values.iter()) {
                write_index_rows(&mut tx, &table, *rint, v).await?;
            }
        }
        // 読んだ世代の待ちだけを消す（処理中に積まれた再要求は残る）。
        let fields: Vec<i16> = pending.iter().map(|p| p.0).collect();
        let records: Vec<i64> = pending.iter().map(|p| p.1).collect();
        let gens: Vec<i64> = pending.iter().map(|p| p.2).collect();
        sqlx::query(
            "DELETE FROM data_derived_pending p USING unnest($3::int2[], $4::int8[], $5::int8[]) \
               AS u(f, r, g) \
             WHERE p.tenant_int = $1 AND p.table_int = $2 AND p.field_id = u.f \
               AND p.record_int = u.r AND p.gen = u.g",
        )
        .bind(table.keys.tenant_int)
        .bind(table.keys.table_int)
        .bind(&fields)
        .bind(&records)
        .bind(&gens)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        tx.commit().await.map_err(map_db)?;
        Ok(pending.len() < usize::try_from(limit).unwrap_or(usize::MAX))
    }

    /// テーブルの再計算待ちを全て処理する（テスト・運用の同期実行用）。
    pub async fn recompute_derived(
        &self,
        tenant_id: &str,
        table_id: Uuid,
    ) -> Result<(), DataError> {
        while !self
            .derived_recompute_step(tenant_id, table_id, RECOMPUTE_CHUNK)
            .await?
        {}
        Ok(())
    }
}
