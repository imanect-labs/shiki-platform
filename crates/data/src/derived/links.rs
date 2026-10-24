//! 双方向リンクの読取と対称列の作成（data-platform §7.1）。
//!
//! - 1 エッジ = 順方向行（参照元 `table_int`）＋逆方向行（参照先 `table_int`・`is_reverse`）。どちらの向きも
//!   **自分側の `table_int` で引く**ので 1 パーティションへ枝刈りされる（[`LINK_TRAVERSAL_SQL`]）。
//! - 対称列（逆リンク）は参照先スキーマ上の**メタデータ**で、値を本体に持たない。順方向の書込と同じ Tx で
//!   逆方向行を書く（[`crate::index::link`]）ので、両方向が食い違う状態は構造的に生じない。
//! - 対称列を作るのはリンク列を宣言したスキーマ保存の Tx（参照先テーブルの owner 権限が要る）。

use std::collections::{BTreeSet, HashMap};

use authz::AuthContext;
use sqlx::types::Json;
use sqlx::PgConnection;
use uuid::Uuid;

use super::types::LinkDef;
use crate::field_key::is_valid_field_key;
use crate::index::state::field_id_i16;
use crate::model::{DataRecord, DataTable, FieldDef, FieldId, FieldType, TableKeys, TableSchema};
use crate::store::DataStore;
use crate::{map_db, DataError};

/// 1 レコードあたりに読む相手の上限（対称列は参照元の数だけ伸びるので、読取の量を有界にする・PIT-72）。
/// 全件が要るときはリンク元の列で絞り込む（`link eq <id>`）。
pub const MAX_LINK_VALUES_PER_RECORD: i64 = 1_000;

/// リンクの走査（行が所属する側の `table_int` で引く・`$4` = 相手側の `table_int`・`$6` = 1 レコードあたりの上限）。
///
/// 順方向（参照元 → 参照先）も逆方向（対称列: 参照先 → 参照元）も同じ形で、`data_link` と相手側の
/// `data_record` がそれぞれ計画時に 1 パーティションへ枝刈りされる（EXPLAIN の回帰テストで固定）。
#[doc(hidden)]
pub const LINK_TRAVERSAL_SQL: &str = "SELECT t.src_record, \
       (SELECT d.id FROM data_record d \
         WHERE d.tenant_int = $1 AND d.table_int = $4 AND d.record_int = t.dst_record) AS dst_id \
     FROM (SELECT l.src_record, l.ord, l.dst_record, \
                  row_number() OVER (PARTITION BY l.src_record ORDER BY l.ord) AS rn \
           FROM data_link l \
           WHERE l.tenant_int = $1 AND l.table_int = $2 AND l.field_id = $3 \
             AND l.src_record = ANY($5::int8[])) t \
     WHERE t.rn <= $6 \
     ORDER BY t.src_record, t.ord";

/// リンクを辿る（`src` の各レコード → 相手側の外部 ID の並び）。相手が削除済みの辺は落とす。
///
/// 1 レコードあたり上限 + 1 件まで返す（上限を超えたかを呼び出し側が判定できるように）。
pub(crate) async fn traverse(
    conn: &mut PgConnection,
    keys: TableKeys,
    field: FieldId,
    dst_table_int: i64,
    src: &[i64],
) -> Result<Vec<(i64, Uuid)>, DataError> {
    if src.is_empty() {
        return Ok(Vec::new());
    }
    // 参照元の record_int の集合は Roaring で重複を除き、段（配列長の上限ごと）に分けて辿る
    // （バッチ駆動・query-optimizer.md §6.4）。
    let src: Vec<i64> = crate::query::planner::batch::bitmap(src.iter().copied())
        .iter()
        .filter_map(|x| i64::try_from(x).ok())
        .collect();
    let fid = field_id_i16(field)?;
    let mut out = Vec::new();
    for stage in crate::query::planner::batch::stages(&src) {
        let rows: Vec<(i64, Option<Uuid>)> = sqlx::query_as(LINK_TRAVERSAL_SQL)
            .bind(keys.tenant_int)
            .bind(keys.table_int)
            .bind(fid)
            .bind(dst_table_int)
            .bind(stage)
            .bind(MAX_LINK_VALUES_PER_RECORD + 1)
            .fetch_all(&mut *conn)
            .await
            .map_err(map_db)?;
        out.extend(rows.into_iter().filter_map(|(s, d)| d.map(|d| (s, d))));
    }
    Ok(out)
}

/// 対称列の値（行ごとの見える参照元）と、上限を超えた行。
#[derive(Debug, Default)]
pub(crate) struct ReverseValues {
    pub values: HashMap<Uuid, Vec<Uuid>>,
    pub truncated: std::collections::HashSet<Uuid>,
    /// 見える参照元の表示名（参照元のタイトル列・順方向のリンクと同じ規則）。
    pub labels: HashMap<Uuid, String>,
}

impl DataStore {
    /// 対称列の値（行ごとの参照元 id の並び）を、**閲覧者に見える参照元だけ**に絞って返す。
    pub(crate) async fn reverse_link_values(
        &self,
        ctx: &AuthContext,
        az: &crate::policy::memo::AuthzMemo<'_>,
        table: &DataTable,
        f: &FieldDef,
        rows: &[DataRecord],
        depth: usize,
    ) -> Result<ReverseValues, DataError> {
        let Some(src_table_id) = f.ref_table else {
            return Ok(ReverseValues::default());
        };
        // 参照元テーブル（対称列から見た相手）。消えていれば辺は無い。
        let Some(src_keys) = self.table_keys_of(&ctx.tenant_id, src_table_id).await? else {
            return Ok(ReverseValues::default());
        };
        let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
        let mut conn = self
            .pools
            .for_tenant(&ctx.tenant_id)
            .acquire()
            .await
            .map_err(map_db)?;
        let ints = Self::record_ints_for(&mut conn, &ctx.tenant_id, table.keys, &ids).await?;
        let by_int: HashMap<i64, Uuid> = ints.iter().map(|(u, i)| (*i, *u)).collect();
        let src: Vec<i64> = by_int.keys().copied().collect();
        let edges = traverse(&mut conn, table.keys, f.id, src_keys.table_int, &src).await?;
        drop(conn);
        // 参照元の行の可視性を閲覧者本人の権限で判定する（テーブル viewer・行述語の二段）。
        let dsts: Vec<Uuid> = edges
            .iter()
            .map(|(_, d)| *d)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let visible = self
            .visible_ref_rows(ctx, az, src_table_id, &dsts, &BTreeSet::new(), depth + 1)
            .await?;
        let mut out = ReverseValues {
            labels: visible.labels(),
            ..ReverseValues::default()
        };
        let mut seen: HashMap<i64, i64> = HashMap::new();
        for (s, d) in edges {
            let n = seen.entry(s).or_default();
            *n += 1;
            let Some(src_id) = by_int.get(&s) else {
                continue;
            };
            if *n > MAX_LINK_VALUES_PER_RECORD {
                // 上限を超えた: 表示は先頭の上限件まで、集計はこの行を「不明」にする（部分集合で
                // 計算した誤った合計を返さない）。
                out.truncated.insert(*src_id);
                continue;
            }
            if visible.rows.contains_key(&d) {
                out.values.entry(*src_id).or_default().push(d);
            }
        }
        Ok(out)
    }
}

/// スキーマ保存の Tx で、新しいリンク列の対称列を参照先テーブルへ作る。
///
/// `table_id` は保存中のテーブル（対称列の `ref_table`）。参照先の `data_table` 行を `FOR UPDATE` で押さえ、
/// 参照先の `next_field_id` から採番してスキーマへ追記し、`schema_version` を進める。作った対称列の
/// `field_id` を保存中のスキーマの `LinkDef.symmetric_field` へ書き戻す。参照先の owner 権限は
/// 呼び出し側が先に確かめること。
pub(crate) async fn create_symmetric_fields(
    conn: &mut PgConnection,
    tenant_id: &str,
    table_id: Uuid,
    schema: &mut TableSchema,
    current: Option<&TableSchema>,
) -> Result<Vec<Uuid>, DataError> {
    let mut touched = Vec::new();
    for f in &mut schema.fields {
        let is_new = current.is_none_or(|c| c.field_by_id(f.id).is_none());
        let (Some(link), Some(ref_table)) = (f.link.as_mut(), f.ref_table) else {
            continue;
        };
        if !is_new || link.reverse || link.symmetric_field.is_some() {
            continue;
        }
        let Some(sym_key) = link.symmetric_key.clone() else {
            continue;
        };
        if ref_table == table_id {
            return Err(crate::schema_invalid(format!(
                "'{}': 自己参照のリンクに対称列は作れません",
                f.key
            )));
        }
        let row: Option<(Json<TableSchema>, i32, i32, i64)> = sqlx::query_as(
            "SELECT schema, next_field_id, tenant_int, table_int FROM data_table \
             WHERE tenant_id = $1 AND id = $2 AND deleted_at IS NULL AND key_format = 2 FOR UPDATE",
        )
        .bind(tenant_id)
        .bind(ref_table)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db)?;
        let Some((Json(mut ref_schema), next_id, tenant_int, ref_table_int)) = row else {
            return Err(crate::schema_invalid(format!(
                "'{}': リンクの参照先テーブルが見つかりません",
                f.key
            )));
        };
        if !is_valid_field_key(&sym_key) || ref_schema.field(&sym_key).is_some() {
            return Err(crate::schema_invalid(format!(
                "'{}': 対称列の key '{sym_key}' は参照先で使えません（不正または重複）",
                f.key
            )));
        }
        let sym_id = FieldId::try_from(next_id)
            .ok()
            .filter(|id| *id <= crate::model::MAX_FIELD_ID)
            .ok_or_else(|| crate::schema_invalid("参照先のフィールド ID が上限に達しています"))?;
        let mut sym = FieldDef::new(sym_key.clone(), FieldType::RecordRef);
        sym.id = sym_id;
        // 作成時に表示名を付けられる（保存の後に参照先の改訂を別に送らなくてよい・省略時は参照名）。
        if let Some(name) = link
            .symmetric_display_name
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
        {
            sym.display_name = name.to_string();
        }
        sym.order = ref_schema.fields.iter().map(|x| x.order).max().unwrap_or(0) + 1;
        sym.ref_table = Some(table_id);
        sym.link = Some(LinkDef {
            cardinality: link.cardinality.mirror(),
            symmetric_field: Some(f.id),
            symmetric_key: None,
            symmetric_display_name: None,
            reverse: true,
        });
        ref_schema.fields.push(sym);
        crate::schema::validate_table_schema(&ref_schema)?;
        sqlx::query(
            "UPDATE data_table SET schema = $3, schema_version = schema_version + 1, \
               next_field_id = $4, updated_at = now() \
             WHERE tenant_id = $1 AND id = $2",
        )
        .bind(tenant_id)
        .bind(ref_table)
        .bind(Json(&ref_schema))
        .bind(next_id + 1)
        .execute(&mut *conn)
        .await
        .map_err(map_db)?;
        // 逆方向行の宣言（single_valued を DB で固定する FK の参照先）。
        crate::index::link::declare_link_fields(
            conn,
            TableKeys {
                tenant_int,
                table_int: ref_table_int,
            },
            &ref_schema,
        )
        .await?;
        link.symmetric_field = Some(sym_id);
        touched.push(ref_table);
    }
    Ok(touched)
}
