//! 索引状態の台帳（`data_index_state`・data-platform §10・PIT-72）。
//!
//! (フィールド, 索引テーブル) ごとに `absent` / `building` / `active` を持つ。
//!
//! ```text
//! absent ──promote──▶ building ──バックフィル完了──▶ active ──demote──▶ absent
//!                        ↑                              ↑
//!               新規書込は write-through            通常運用
//! ```
//!
//! # 競合しない理由（バックフィルとの並行書込）
//!
//! - 書込 Tx は**先頭で** `data_table` 行を `FOR KEY SHARE` で押さえてから台帳を読む（[`lock_plan`]）。
//! - `absent → building` の遷移（[`sync_ledger`]）は同じ行を `FOR UPDATE` で押さえた Tx でだけ行う。
//!   `FOR UPDATE` は `FOR KEY SHARE` と衝突するので、**遷移前に台帳を読んだ書込は遷移のコミットより先に
//!   必ず終わる**。遷移後に始まった書込は `building` を見て write-through する。
//! - バックフィルは遷移のコミット後に始まり、レコードを `FOR SHARE` で読む（書込の `FOR UPDATE` と衝突）
//!   ので、走査済みのレコードへの後続の書込は write-through で、走査前の書込は走査で拾われる。
//!   したがって完成後の索引から変更が欠落しない（§10「building 中は write-through する」）。
//! - `building → active` は台帳の行を書き換えるだけ（書込は両状態とも保守するので順序を問わない）。

use std::collections::{BTreeMap, BTreeSet};

use sqlx::PgConnection;
use uuid::Uuid;

use super::{IndexState, IndexTable};
use crate::model::{
    FieldId, FieldType, TableKeys, TableSchema, SYSTEM_FIELD_CREATED_AT, SYSTEM_FIELD_OWNER,
    SYSTEM_FIELD_UPDATED_AT,
};
use crate::{map_db, DataError};

/// スキーマ宣言が要求する (フィールド, 索引テーブル) の集合。
///
/// - 擬似列 owner / created_at / updated_at は常に `data_index_text`（全レコードに 1 行）
/// - `record_ref` は常に `data_link`（リンクはオプトインではない・カーディナリティを DB で強制する）
/// - `indexed || unique` は型に応じた値索引（number = num、それ以外 = text。filter/sort の到達可能性は
///   `indexed || unique` で判定するため unique 列にも値索引を持たせる・§5.2）
/// - `unique` は `data_unique`、`searchable`（text のみ）は `data_index_search`
/// - lookup / computed は索引しない（書込不可・派生値の材料化は Task 15.4）
pub(crate) fn desired(schema: &TableSchema) -> BTreeSet<(FieldId, IndexTable)> {
    let mut out = BTreeSet::new();
    for sys in [
        SYSTEM_FIELD_OWNER,
        SYSTEM_FIELD_CREATED_AT,
        SYSTEM_FIELD_UPDATED_AT,
    ] {
        out.insert((sys, IndexTable::Text));
    }
    for f in &schema.fields {
        match f.field_type {
            FieldType::Lookup | FieldType::Computed => continue,
            FieldType::RecordRef => {
                out.insert((f.id, IndexTable::Link));
            }
            FieldType::Number if f.indexed || f.unique => {
                out.insert((f.id, IndexTable::Num));
            }
            _ if f.indexed || f.unique => {
                out.insert((f.id, IndexTable::Text));
            }
            _ => {}
        }
        if f.unique {
            out.insert((f.id, IndexTable::Unique));
        }
        if f.searchable && f.field_type == FieldType::Text {
            out.insert((f.id, IndexTable::Search));
        }
    }
    out
}

/// 書込経路が保守する索引（`building` / `active`）。書込 Tx の間だけ有効。
#[derive(Debug, Clone, Default)]
pub(crate) struct IndexPlan {
    pub(crate) keys: TableKeys,
    maintained: BTreeMap<FieldId, Vec<IndexTable>>,
}

impl IndexPlan {
    /// フィールドについて保守する索引テーブル（無ければ空）。
    pub(crate) fn tables_for(&self, field: FieldId) -> &[IndexTable] {
        self.maintained.get(&field).map_or(&[], Vec::as_slice)
    }

    /// 保守対象のフィールド（擬似列を含む）。
    pub(crate) fn fields(&self) -> impl Iterator<Item = (FieldId, &[IndexTable])> {
        self.maintained.iter().map(|(f, t)| (*f, t.as_slice()))
    }

    /// キーだけの計画（バックフィルは対象の組を明示して書くので保守対象を持たない）。
    pub(crate) fn keys_only(keys: TableKeys) -> Self {
        IndexPlan {
            keys,
            maintained: BTreeMap::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(keys: TableKeys, entries: &[(FieldId, IndexTable)]) -> Self {
        let mut maintained: BTreeMap<FieldId, Vec<IndexTable>> = BTreeMap::new();
        for (f, t) in entries {
            maintained.entry(*f).or_default().push(*t);
        }
        IndexPlan { keys, maintained }
    }
}

/// 書込 Tx の**先頭で**呼ぶ: `data_table` 行を `FOR KEY SHARE` で押さえ、保守対象を台帳から読む。
///
/// レコード行のロックより先に呼ぶこと（ロック順: テーブル行 → レコード行。スキーマ改訂は
/// テーブル行の `FOR UPDATE` で、ここと直列化される）。削除済み・不在のテーブルは `NotFound`。
///
/// `schema_version` は呼び出し側が検証に使ったスキーマの版。押さえた行の版と違えば（検証の後に
/// スキーマが改訂された）409 にする。古いスキーマで索引を書くと、改訂で増えた索引の行を
/// 取りこぼす・消し損なうため。
pub(crate) async fn lock_plan(
    conn: &mut PgConnection,
    tenant_id: &str,
    table_id: Uuid,
    keys: TableKeys,
    schema_version: i64,
) -> Result<IndexPlan, DataError> {
    let live: Option<i64> = sqlx::query_scalar(
        "SELECT schema_version FROM data_table \
         WHERE tenant_id = $1 AND id = $2 AND deleted_at IS NULL FOR KEY SHARE",
    )
    .bind(tenant_id)
    .bind(table_id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db)?;
    match live {
        None => return Err(DataError::NotFound),
        Some(v) if v != schema_version => {
            return Err(DataError::Conflict(
                "テーブルのスキーマが更新されました。再試行してください".into(),
            ));
        }
        Some(_) => {}
    }
    let mut maintained: BTreeMap<FieldId, Vec<IndexTable>> = BTreeMap::new();
    for (field, table, state) in load_states(conn, keys).await? {
        if state.is_maintained() {
            maintained.entry(field).or_default().push(table);
        }
    }
    Ok(IndexPlan { keys, maintained })
}

/// 台帳の全行（状態つき）。Task 15.3 のクエリコンパイラは `active` だけを駆動・フィルタに使う
/// （`building` は書込だけ受ける。半端な索引で結果を欠落させない・§10）。
pub(crate) async fn load_states(
    conn: &mut PgConnection,
    keys: TableKeys,
) -> Result<Vec<(FieldId, IndexTable, IndexState)>, DataError> {
    let rows: Vec<(i16, i16, String)> = sqlx::query_as(
        "SELECT field_id, index_table, state FROM data_index_state \
         WHERE tenant_int = $1 AND table_int = $2 ORDER BY field_id, index_table",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db)?;
    rows.into_iter()
        .map(|(f, t, s)| {
            let field = FieldId::try_from(f)
                .map_err(|_| DataError::Internal(format!("台帳の field_id {f} が不正です")))?;
            let table = IndexTable::from_code(t)
                .ok_or_else(|| DataError::Internal(format!("台帳の index_table {t} が不正です")))?;
            let state = IndexState::parse(&s)
                .ok_or_else(|| DataError::Internal(format!("台帳の state '{s}' が不正です")))?;
            Ok((field, table, state))
        })
        .collect()
}

/// 台帳をスキーマ宣言へ合わせる（作成・改訂・初期化の Tx 上・`data_table` 行は `FOR UPDATE` 済み）。
///
/// - 宣言されたのに保守していない → `has_rows` なら `building`（バックフィルが要る）、空なら `active`
/// - 宣言から外れた → `absent` にして索引行を同じ Tx で消す
///
/// `building` が 1 つでも残っていれば `true`（呼び出し側がバックフィルを積む）。
pub(crate) async fn sync_ledger(
    conn: &mut PgConnection,
    keys: TableKeys,
    schema: &TableSchema,
    has_rows: bool,
) -> Result<bool, DataError> {
    let want = desired(schema);
    let current = load_states(conn, keys).await?;
    let maintained: BTreeSet<(FieldId, IndexTable)> = current
        .iter()
        .filter(|(_, _, s)| s.is_maintained())
        .map(|(f, t, _)| (*f, *t))
        .collect();

    let start = if has_rows {
        IndexState::Building
    } else {
        IndexState::Active
    };
    for (field, table) in want.difference(&maintained) {
        set_state(conn, keys, *field, *table, start).await?;
    }
    for (field, table) in maintained.difference(&want) {
        set_state(conn, keys, *field, *table, IndexState::Absent).await?;
        delete_field_rows(conn, keys, *field, *table).await?;
    }
    super::link::declare_link_fields(conn, keys, schema).await?;
    let building = load_states(conn, keys)
        .await?
        .iter()
        .any(|(_, _, s)| *s == IndexState::Building);
    Ok(building)
}

pub(crate) async fn set_state(
    conn: &mut PgConnection,
    keys: TableKeys,
    field: FieldId,
    table: IndexTable,
    state: IndexState,
) -> Result<(), DataError> {
    sqlx::query(
        "INSERT INTO data_index_state (tenant_int, table_int, field_id, index_table, state) \
         VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (tenant_int, table_int, field_id, index_table) \
         DO UPDATE SET state = excluded.state, updated_at = now()",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .bind(field_id_i16(field)?)
    .bind(table.code())
    .bind(state.as_str())
    .execute(&mut *conn)
    .await
    .map_err(map_db)?;
    Ok(())
}

/// `building` の全行を `active` へ切り替える（バックフィル完了時・原子的）。
pub(crate) async fn activate_building(
    conn: &mut PgConnection,
    keys: TableKeys,
) -> Result<u64, DataError> {
    Ok(sqlx::query(
        "UPDATE data_index_state SET state = 'active', updated_at = now() \
         WHERE tenant_int = $1 AND table_int = $2 AND state = 'building'",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .execute(&mut *conn)
    .await
    .map_err(map_db)?
    .rows_affected())
}

/// 1 フィールド × 1 索引テーブルの索引行を全て消す（降格）。
async fn delete_field_rows(
    conn: &mut PgConnection,
    keys: TableKeys,
    field: FieldId,
    table: IndexTable,
) -> Result<(), DataError> {
    // 表名は固定集合（IndexTable::relation）からのみ埋め込む。値はバインド（PIT-21）。
    let sql = format!(
        "DELETE FROM {} WHERE tenant_int = $1 AND table_int = $2 AND field_id = $3",
        table.relation()
    );
    sqlx::query(&sql)
        .bind(keys.tenant_int)
        .bind(keys.table_int)
        .bind(field_id_i16(field)?)
        .execute(&mut *conn)
        .await
        .map_err(map_db)?;
    Ok(())
}

pub(crate) fn field_id_i16(field: FieldId) -> Result<i16, DataError> {
    i16::try_from(field).map_err(|_| {
        DataError::Invalid(format!(
            "フィールド ID {field} が上限（{}）を超えています",
            crate::model::MAX_FIELD_ID
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::FieldDef;

    fn schema(fields: Vec<FieldDef>) -> TableSchema {
        TableSchema {
            fields,
            status_field: None,
            row_policy: None,
            field_policy: vec![],
            aggregate_min_rows: None,
            fsm_ref: None,
        }
    }

    fn f(id: FieldId, key: &str, ty: FieldType) -> FieldDef {
        let mut d = FieldDef::new(key, ty);
        d.id = id;
        d
    }

    #[test]
    fn desired_covers_system_columns_links_and_declared_fields() {
        let mut title = f(16, "title", FieldType::Text);
        title.indexed = true;
        title.searchable = true;
        let mut amount = f(17, "amount", FieldType::Number);
        amount.unique = true;
        let mut tags = f(18, "tags", FieldType::MultiSelect);
        tags.indexed = true;
        let mut cust = f(19, "customer", FieldType::RecordRef);
        cust.ref_table = Some(Uuid::nil());
        let memo = f(20, "memo", FieldType::Text); // 未宣言 → 索引しない
        let got = desired(&schema(vec![title, amount, tags, cust, memo]));
        let want: BTreeSet<(FieldId, IndexTable)> = [
            (0, IndexTable::Text),
            (1, IndexTable::Text),
            (2, IndexTable::Text),
            (16, IndexTable::Text),
            (16, IndexTable::Search),
            (17, IndexTable::Num),
            (17, IndexTable::Unique),
            (18, IndexTable::Text),
            (19, IndexTable::Link),
        ]
        .into_iter()
        .collect();
        assert_eq!(got, want);
    }

    #[test]
    fn index_table_codes_roundtrip_and_relations_are_fixed() {
        for t in IndexTable::ALL {
            assert_eq!(IndexTable::from_code(t.code()), Some(t));
            assert!(t.relation().starts_with("data_"));
        }
        assert_eq!(IndexTable::from_code(0), None);
        assert_eq!(IndexTable::Link.record_column(), "src_record");
        for s in [IndexState::Absent, IndexState::Building, IndexState::Active] {
            assert_eq!(IndexState::parse(s.as_str()), Some(s));
        }
        assert!(!IndexState::Absent.is_maintained());
        assert!(
            IndexState::Building.is_maintained(),
            "building は write-through"
        );
    }
}
