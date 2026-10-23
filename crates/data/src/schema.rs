//! テーブルスキーマの検証（登録時・改訂時）。
//!
//! フィールドの参照名（`key`）は `^[a-z][a-z0-9_]{0,63}$` に厳格制限する。SQL（式インデックス
//! DDL・JSONB アクセサ）へ埋め込むのは `key` ではなく内部 ID 由来の `"f{id}"` だけ
//! （[`crate::field_key`]・Task 15.1）。検証は `id` 採番後のスキーマに対して行う。

use std::collections::HashSet;

use crate::field_key::{is_valid_field_key, validate_identity};
use crate::model::{ComputedOp, FieldDef, FieldType, TableSchema};
use crate::DataError;

/// テーブル名の上限長（マニフェスト束縛・URL パスでの扱いやすさを優先）。
const MAX_TABLE_NAME_LEN: usize = 128;
/// 1 テーブルのフィールド数上限（式インデックス数・検証コストの防御的上限）。
const MAX_FIELDS: usize = 200;
/// select / multi_select の選択肢数上限。
const MAX_OPTIONS: usize = 200;
/// 選択肢 1 件の長さ上限。
const MAX_OPTION_LEN: usize = 128;

/// テーブル名を検証する（trim 済みを返す）。
pub(crate) fn validate_table_name(name: &str) -> Result<&str, DataError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(DataError::Invalid("name が空です".into()));
    }
    if name.len() > MAX_TABLE_NAME_LEN {
        return Err(DataError::Invalid(format!(
            "name が長すぎます（最大 {MAX_TABLE_NAME_LEN} 文字）"
        )));
    }
    Ok(name)
}

/// スキーマ全体を検証する（登録時・ミニアプリのマニフェスト検証など外部から使う入口）。
///
/// `id` 未採番の入力も受ける（新規作成と同じ規則で採番した複製を検証する）。
pub fn validate_table_schema_public(schema: &TableSchema) -> Result<(), DataError> {
    let mut assigned = schema.clone();
    crate::field_key::assign_field_ids(None, &mut assigned)?;
    // 材料化可否・値の型はサーバが参照先を読んで決める（入力の宣言は信用しない）。ここでは決められない
    // ので、派生フィールドの indexed は拒否側に倒す（作成時の検証で改めて判定する）。
    for f in &mut assigned.fields {
        f.materialization = None;
        f.result_type = None;
    }
    validate_table_schema(&assigned)
}

pub(crate) fn validate_table_schema(schema: &TableSchema) -> Result<(), DataError> {
    if schema.fields.is_empty() {
        return Err(DataError::Invalid("fields が空です".into()));
    }
    if schema.fields.len() > MAX_FIELDS {
        return Err(DataError::Invalid(format!(
            "フィールド数が多すぎます（最大 {MAX_FIELDS}）"
        )));
    }
    let mut keys = HashSet::new();
    for f in &schema.fields {
        if !is_valid_field_key(&f.key) {
            return Err(DataError::Invalid(format!(
                "フィールドの key '{}' が不正です（^[a-z][a-z0-9_]{{0,63}}$）",
                f.key
            )));
        }
        if !keys.insert(f.key.as_str()) {
            return Err(DataError::Invalid(format!(
                "フィールドの key '{}' が重複しています",
                f.key
            )));
        }
        validate_field(schema, f)?;
    }
    validate_identity(schema)?;
    if let Some(policy) = &schema.row_policy {
        crate::policy::validate::validate_row_policy(schema, policy)?;
    }
    for fp in &schema.field_policy {
        if schema.field(&fp.field).is_none() {
            return Err(DataError::Invalid(format!(
                "field_policy の対象 '{}' が fields にありません",
                fp.field
            )));
        }
        crate::policy::validate::validate_role_level_expr(&fp.readable_by, "field_policy")?;
    }
    // 検索の投影テンプレート（Task 15.8）: マスク対象列・派生列を含めない（fail-closed）。
    crate::rag::projection::validate_projection(schema)?;
    if let Some(k) = schema.aggregate_min_rows {
        if k < 1 {
            return Err(DataError::Invalid(
                "aggregate_min_rows は 1 以上で指定してください".into(),
            ));
        }
    }
    // FSM 参照があれば status_field は必須（遷移対象の状態フィールドが要る）。
    // FSM 定義本体（states/transitions）の検証は fsm ストア保存時に行う。
    if schema.fsm_ref.is_some() && schema.status_field.is_none() {
        return Err(DataError::Invalid(
            "fsm_ref を持つテーブルは status_field が必須です".into(),
        ));
    }
    if let Some(status) = &schema.status_field {
        let f = schema.field(status).ok_or_else(|| {
            DataError::Invalid(format!("status_field '{status}' が fields にありません"))
        })?;
        if f.field_type != FieldType::Select {
            return Err(DataError::Invalid(format!(
                "status_field '{status}' は select 型である必要があります"
            )));
        }
    }
    Ok(())
}

/// 単一フィールド定義の型別検証。
fn validate_field(schema: &TableSchema, f: &FieldDef) -> Result<(), DataError> {
    let name = &f.key;
    // number の精度宣言（data-platform §4.2）。number 以外への指定・scale 範囲外・min > max は 422。
    crate::numeric::validate_spec(f)?;
    // リンク定義・ロールアップ・数式（Task 15.4）。
    crate::derived::schema::validate_derived_field(schema, f)?;
    // 型ごとの付帯定義。
    match f.field_type {
        FieldType::Select | FieldType::MultiSelect => {
            if f.options.is_empty() || f.options.len() > MAX_OPTIONS {
                return Err(DataError::Invalid(format!(
                    "'{name}': options は 1〜{MAX_OPTIONS} 件で指定してください"
                )));
            }
            let mut seen = HashSet::new();
            for o in &f.options {
                if o.is_empty() || o.len() > MAX_OPTION_LEN {
                    return Err(DataError::Invalid(format!(
                        "'{name}': 選択肢は 1〜{MAX_OPTION_LEN} 文字で指定してください"
                    )));
                }
                if !seen.insert(o.as_str()) {
                    return Err(DataError::Invalid(format!(
                        "'{name}': 選択肢 '{o}' が重複しています"
                    )));
                }
            }
        }
        FieldType::RecordRef => {
            if f.ref_table.is_none() {
                return Err(DataError::Invalid(format!(
                    "'{name}': record_ref には ref_table が必須です"
                )));
            }
        }
        FieldType::Lookup => {
            let def = f.lookup.as_ref().ok_or_else(|| {
                DataError::Invalid(format!("'{name}': lookup には lookup 定義が必須です"))
            })?;
            let via = schema.field(&def.via_field).ok_or_else(|| {
                DataError::Invalid(format!(
                    "'{name}': lookup.via_field '{}' が fields にありません",
                    def.via_field
                ))
            })?;
            if via.field_type != FieldType::RecordRef {
                return Err(DataError::Invalid(format!(
                    "'{name}': lookup.via_field '{}' は record_ref 型である必要があります",
                    def.via_field
                )));
            }
            if !is_valid_field_key(&def.target_field) {
                return Err(DataError::Invalid(format!(
                    "'{name}': lookup.target_field が不正です"
                )));
            }
        }
        FieldType::Computed => {
            let def = f.computed.as_ref().ok_or_else(|| {
                DataError::Invalid(format!("'{name}': computed には computed 定義が必須です"))
            })?;
            if def.fields.is_empty() {
                return Err(DataError::Invalid(format!(
                    "'{name}': computed.fields が空です"
                )));
            }
            let want = match def.op {
                ComputedOp::Sum => FieldType::Number,
                ComputedOp::Concat => FieldType::Text,
            };
            for src in &def.fields {
                let sf = schema.field(src).ok_or_else(|| {
                    DataError::Invalid(format!(
                        "'{name}': computed.fields の '{src}' が fields にありません"
                    ))
                })?;
                if sf.field_type != want {
                    return Err(DataError::Invalid(format!(
                        "'{name}': computed({:?}) の対象 '{src}' は {want:?} 型である必要があります",
                        def.op
                    )));
                }
            }
        }
        _ => {}
    }
    // 派生フィールド（書込不可）に unique/required は意味を持たないため拒否する。indexed は
    // 材料化できる（invariant）列だけ（validate_derived_field・data-platform §7.4）。
    if f.field_type.is_derived() && (f.unique || f.required) {
        return Err(DataError::Invalid(format!(
            "'{name}': 派生フィールド（lookup/computed/rollup/formula）に required/unique は指定できません"
        )));
    }
    // unique は式インデックスで強制できるスカラー型のみ。
    if f.unique
        && !matches!(
            f.field_type,
            FieldType::Text
                | FieldType::Number
                | FieldType::Date
                | FieldType::DateTime
                | FieldType::Select
                | FieldType::UserRef
                | FieldType::RoleRef
                | FieldType::RecordRef
        )
    {
        return Err(DataError::Invalid(format!(
            "'{name}': この型に unique は指定できません"
        )));
    }
    Ok(())
}

/// スキーマ改訂の互換性検証（additive のみ・Task 9.2）。
///
/// `next` は [`crate::field_key::assign_field_ids`] で `current` と照合・採番済みであること
/// （同じ `id` は同じ `key`＝key 不変はそこで強制済み）。既存フィールドの**型変更・削除は拒否**
/// する（保存済み JSONB と式インデックスの整合を崩さない）。追加・required/unique/indexed/
/// options・**display_name / order / description の変更は許す**（表示名の変更はスキーマ 1 箇所の
/// 書き換えで完結し、レコードは 1 行も動かない・data-platform §4.2）。
pub(crate) fn validate_schema_update(
    current: &TableSchema,
    next: &TableSchema,
) -> Result<(), DataError> {
    validate_table_schema(next)?;
    for cur in &current.fields {
        let Some(nf) = next.field_by_id(cur.id) else {
            return Err(DataError::Invalid(format!(
                "フィールド '{}' は削除できません（additive 変更のみ）",
                cur.key
            )));
        };
        if nf.key != cur.key {
            return Err(DataError::Invalid(format!(
                "フィールド id {} の key は変更できません（'{}' → '{}'）",
                cur.id, cur.key, nf.key
            )));
        }
        if nf.field_type != cur.field_type {
            return Err(DataError::Invalid(format!(
                "フィールド '{}' の型は変更できません",
                cur.key
            )));
        }
        // 型固有の定義（参照先・派生定義）も不変（保存済み値の解釈が変わるため）。
        if nf.ref_table != cur.ref_table {
            return Err(DataError::Invalid(format!(
                "フィールド '{}' の ref_table は変更できません",
                cur.key
            )));
        }
        // 精度宣言も不変（保存済みの値と索引は宣言時の規則で正規化されている。変えるなら
        // 既存値の再正規化が要るため、改訂経路では受けない）。
        if nf.numeric_spec != cur.numeric_spec {
            return Err(DataError::Invalid(format!(
                "フィールド '{}' の numeric_spec は変更できません",
                cur.key
            )));
        }
        if nf.lookup != cur.lookup
            || nf.computed != cur.computed
            || nf.rollup != cur.rollup
            || nf.formula != cur.formula
        {
            return Err(DataError::Invalid(format!(
                "フィールド '{}' の lookup/computed/rollup/formula 定義は変更できません",
                cur.key
            )));
        }
        // リンク定義（カーディナリティ・対称列）も不変（変えるなら既存行の再検証が要る・§7.1）。
        if nf.link != cur.link {
            return Err(DataError::Invalid(format!(
                "フィールド '{}' の link は変更できません",
                cur.key
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "schema_tests.rs"]
mod tests;
