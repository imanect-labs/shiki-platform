//! テーブルスキーマの検証（登録時・改訂時）。
//!
//! フィールド名は式インデックス DDL（[`crate::index`]）と JSONB アクセサに埋め込むため、
//! `^[a-z][a-z0-9_]{0,63}$` に厳格制限する（SQL への埋め込みを構造的に安全化）。

use std::collections::HashSet;

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

/// フィールド名が識別子規約（`^[a-z][a-z0-9_]{0,63}$`）を満たすか。
pub(crate) fn is_valid_field_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > 64 {
        return false;
    }
    if !bytes[0].is_ascii_lowercase() {
        return false;
    }
    bytes
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_')
}

/// スキーマ全体を検証する（登録時）。
pub fn validate_table_schema_public(schema: &TableSchema) -> Result<(), DataError> {
    validate_table_schema(schema)
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
    let mut names = HashSet::new();
    for f in &schema.fields {
        if !is_valid_field_name(&f.name) {
            return Err(DataError::Invalid(format!(
                "フィールド名 '{}' が不正です（^[a-z][a-z0-9_]{{0,63}}$）",
                f.name
            )));
        }
        if !names.insert(f.name.as_str()) {
            return Err(DataError::Invalid(format!(
                "フィールド名 '{}' が重複しています",
                f.name
            )));
        }
        validate_field(schema, f)?;
    }
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
    let name = &f.name;
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
            if !is_valid_field_name(&def.target_field) {
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
    // 派生フィールド（書込不可）に unique/required/indexed は意味を持たないため拒否する。
    if matches!(f.field_type, FieldType::Lookup | FieldType::Computed)
        && (f.unique || f.required || f.indexed)
    {
        return Err(DataError::Invalid(format!(
            "'{name}': lookup/computed に required/unique/indexed は指定できません"
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
/// 既存フィールドの**型変更・削除は拒否**する（保存済み JSONB と式インデックスの整合を
/// 崩さない）。追加・required/unique/indexed/options の変更は許す（options の縮小で
/// 既存値が外れるのは読み出しに影響しないため許容。厳格化は書込時にのみ効く）。
pub(crate) fn validate_schema_update(
    current: &TableSchema,
    next: &TableSchema,
) -> Result<(), DataError> {
    validate_table_schema(next)?;
    for cur in &current.fields {
        let Some(nf) = next.field(&cur.name) else {
            return Err(DataError::Invalid(format!(
                "フィールド '{}' は削除できません（additive 変更のみ）",
                cur.name
            )));
        };
        if nf.field_type != cur.field_type {
            return Err(DataError::Invalid(format!(
                "フィールド '{}' の型は変更できません",
                cur.name
            )));
        }
        // 型固有の定義（参照先・派生定義）も不変（保存済み値の解釈が変わるため）。
        if nf.ref_table != cur.ref_table {
            return Err(DataError::Invalid(format!(
                "フィールド '{}' の ref_table は変更できません",
                cur.name
            )));
        }
        if nf.lookup != cur.lookup || nf.computed != cur.computed {
            return Err(DataError::Invalid(format!(
                "フィールド '{}' の lookup/computed 定義は変更できません",
                cur.name
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ComputedDef, LookupDef};

    fn field(name: &str, ty: FieldType) -> FieldDef {
        FieldDef {
            name: name.into(),
            field_type: ty,
            required: false,
            unique: false,
            indexed: false,
            options: vec![],
            ref_table: None,
            lookup: None,
            computed: None,
        }
    }

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

    #[test]
    fn field_name_rules() {
        assert!(is_valid_field_name("title"));
        assert!(is_valid_field_name("a1_b2"));
        assert!(!is_valid_field_name(""));
        assert!(!is_valid_field_name("Title")); // 大文字
        assert!(!is_valid_field_name("1abc")); // 数字始まり
        assert!(!is_valid_field_name("a'; drop")); // 記号
        assert!(!is_valid_field_name(&"x".repeat(65))); // 長すぎ
    }

    #[test]
    fn rejects_duplicate_and_empty() {
        assert!(validate_table_schema(&schema(vec![])).is_err());
        let dup = schema(vec![
            field("a", FieldType::Text),
            field("a", FieldType::Text),
        ]);
        assert!(validate_table_schema(&dup).is_err());
    }

    #[test]
    fn select_requires_options() {
        let mut f = field("status", FieldType::Select);
        assert!(validate_table_schema(&schema(vec![f.clone()])).is_err());
        f.options = vec!["open".into(), "closed".into()];
        assert!(validate_table_schema(&schema(vec![f])).is_ok());
    }

    #[test]
    fn record_ref_requires_ref_table() {
        let f = field("parent", FieldType::RecordRef);
        assert!(validate_table_schema(&schema(vec![f])).is_err());
    }

    #[test]
    fn lookup_requires_record_ref_via() {
        let mut via = field("customer", FieldType::RecordRef);
        via.ref_table = Some(uuid::Uuid::new_v4());
        let mut lk = field("customer_name", FieldType::Lookup);
        lk.lookup = Some(LookupDef {
            via_field: "customer".into(),
            target_field: "name".into(),
        });
        assert!(validate_table_schema(&schema(vec![via.clone(), lk.clone()])).is_ok());
        // via が record_ref でないと拒否。
        lk.lookup = Some(LookupDef {
            via_field: "title".into(),
            target_field: "name".into(),
        });
        let s = schema(vec![field("title", FieldType::Text), via, lk]);
        assert!(validate_table_schema(&s).is_err());
    }

    #[test]
    fn computed_type_must_match_op() {
        let mut c = field("total", FieldType::Computed);
        c.computed = Some(ComputedDef {
            op: ComputedOp::Sum,
            fields: vec!["price".into()],
        });
        // 対象が text だと拒否。
        let bad = schema(vec![field("price", FieldType::Text), c.clone()]);
        assert!(validate_table_schema(&bad).is_err());
        let ok = schema(vec![field("price", FieldType::Number), c]);
        assert!(validate_table_schema(&ok).is_ok());
    }

    #[test]
    fn derived_fields_reject_flags() {
        let mut via = field("customer", FieldType::RecordRef);
        via.ref_table = Some(uuid::Uuid::new_v4());
        let mut lk = field("customer_name", FieldType::Lookup);
        lk.lookup = Some(LookupDef {
            via_field: "customer".into(),
            target_field: "name".into(),
        });
        lk.indexed = true;
        assert!(validate_table_schema(&schema(vec![via, lk])).is_err());
    }

    #[test]
    fn unique_rejected_on_multi_select_and_file() {
        let mut ms = field("tags", FieldType::MultiSelect);
        ms.options = vec!["a".into()];
        ms.unique = true;
        assert!(validate_table_schema(&schema(vec![ms])).is_err());
        let mut fr = field("attachment", FieldType::FileRef);
        fr.unique = true;
        assert!(validate_table_schema(&schema(vec![fr])).is_err());
    }

    #[test]
    fn status_field_must_be_select() {
        let mut s = schema(vec![field("state", FieldType::Text)]);
        s.status_field = Some("state".into());
        assert!(validate_table_schema(&s).is_err());
        let mut sel = field("state", FieldType::Select);
        sel.options = vec!["draft".into(), "done".into()];
        let mut s = schema(vec![sel]);
        s.status_field = Some("state".into());
        assert!(validate_table_schema(&s).is_ok());
    }

    #[test]
    fn schema_update_is_additive_only() {
        let cur = schema(vec![field("title", FieldType::Text)]);
        // 追加は OK。
        let next = schema(vec![
            field("title", FieldType::Text),
            field("n", FieldType::Number),
        ]);
        assert!(validate_schema_update(&cur, &next).is_ok());
        // 削除は拒否。
        assert!(
            validate_schema_update(&cur, &schema(vec![field("n", FieldType::Number)])).is_err()
        );
        // 型変更は拒否。
        assert!(
            validate_schema_update(&cur, &schema(vec![field("title", FieldType::Number)])).is_err()
        );
    }
}
