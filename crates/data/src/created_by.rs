//! 作成者の列（[`FieldType::CreatedBy`]・D-122・BUG-148）。
//!
//! 状態の進め方の「申請した本人は除く」（D-115）を、だれでも書けるメンバーの列で判定すると、承認者を兼ねる
//! 編集者が申請者の列をほかの人にして自分の申請を承認できる。作成者の列は、値をレコードの作成者
//! （`data_record.owner`・作成した主体・どの入口からも書き換えられない）に固定して、この迂回を塞ぐ。
//! 行の決まりの語彙（`PolicyExpr`）は変えない（`field_cmp(作成者の列, ne, $user.id)` と書く）。
//!
//! - **値**: 保存しない（[`crate::field_key::encode_record`] / [`crate::field_key::decode_record`] は扱わない）。
//!   外へ返す出口（[`DataStore::project_read`] と作成の応答）と、派生列の参照先の行で、作成者を入れる（[`fill`]）。
//!   列を足す前に作ったレコードも、作成者で埋まって見える。
//! - **書けない**: 作成・更新・一括取込（JSON・CSV）・ワークフロー・チャット・スクリプト・ゲートウェイの
//!   どの入口でも、値を書こうとしたら 400（[`write_rejected`]・黙って捨てない）。
//! - **条件・並べ替え・集計**: owner の擬似列（`field_id = 0`・全レコードに 1 行）で引く（`query::bound`）。
//!   列の索引は持たない（`index::state::desired` に載せない）。`indexed` は常に立てて返す（いつでも使える）。
//! - **行の決まり**: `field_cmp` の比べは、本体の SQL（`policy::compile`）・索引の SQL（`policy::compile_index`）・
//!   Rust の事後検査（`query::planner::verify`）・SSE の購読者ごとの判定（`stream::visibility`）のどれでも
//!   owner と比べる。owner は空にならないので、値なしの `ne` が偽になる（誰も書き換えられなくなる）レコードは無い。
//! - 必須・一意・部分一致検索・フィールドマスク・検索（RAG）の投影には使えない（スキーマの検証で 400）。
//!   作成者はレコードを読める人にいつも見えるため（レコードの応答の `owner`）、隠しても意味がない。
//!
//! [`DataStore::project_read`]: crate::store::DataStore

use crate::model::{DataRecord, FieldDef, FieldType, TableSchema};
use crate::{DataError, InvalidCode};

/// 作成者の列があるか。
pub(crate) fn any(schema: &TableSchema) -> bool {
    schema.fields.iter().any(FieldDef::is_created_by)
}

/// レコードの作成者の列に作成者を入れる（外へ返す出口と、派生列の参照先の行で使う）。
pub(crate) fn fill(schema: &TableSchema, records: &mut [DataRecord]) {
    if !any(schema) {
        return;
    }
    for r in records {
        let owner = serde_json::Value::String(r.owner.clone());
        if let Some(obj) = r.data.as_object_mut() {
            for f in schema.fields.iter().filter(|f| f.is_created_by()) {
                obj.insert(f.key.clone(), owner.clone());
            }
        }
    }
}

/// 作成者の列へ値を書こうとしたときの 400（列は書き手が名指ししたもの・文言は表示名へ直す）。
pub(crate) fn write_rejected(f: &FieldDef) -> DataError {
    let e = DataError::input_detail(
        InvalidCode::InvalidInput,
        Some(&f.key),
        format!(
            "'{}' には書き込めません。作成者の列には、レコードを追加した人が自動で入ります",
            f.key
        ),
    );
    match e {
        DataError::InvalidInput(ie) => {
            DataError::InvalidInput(Box::new(ie.relabel(&f.key, &f.display_name)))
        }
        other => other,
    }
}

/// 作成者の列の定義の検証（スキーマの保存時）。
pub(crate) fn validate_field(f: &FieldDef) -> Result<(), DataError> {
    if f.field_type != FieldType::CreatedBy {
        return Ok(());
    }
    if f.required || f.unique {
        return Err(crate::schema_invalid(format!(
            "'{}': 作成者の列には必須・重複なしを指定できません（レコードを追加した人がいつも入ります）",
            f.key
        )));
    }
    Ok(())
}

/// 作成者の列の正規化（スキーマの保存時）: `indexed` は常に立てる（owner の擬似列で絞り込み・並べ替えに
/// 使える。列の索引は作らない）。
pub(crate) fn normalize(schema: &mut TableSchema) {
    for f in schema.fields.iter_mut().filter(|f| f.is_created_by()) {
        f.indexed = true;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> TableSchema {
        let mut by = FieldDef::new("made_by", FieldType::CreatedBy);
        by.display_name = "作成者".into();
        TableSchema {
            fields: vec![FieldDef::new("title", FieldType::Text), by],
            status_field: None,
            row_policy: None,
            field_policy: vec![],
            aggregate_min_rows: None,
            fsm_ref: None,
            rag: None,
        }
    }

    fn record(owner: &str, data: serde_json::Value) -> DataRecord {
        DataRecord {
            id: uuid::Uuid::new_v4(),
            table_id: uuid::Uuid::new_v4(),
            data,
            rev: 1,
            owner: owner.into(),
            created_at: chrono::DateTime::UNIX_EPOCH,
            updated_at: chrono::DateTime::UNIX_EPOCH,
            suppressed_fields: Vec::new(),
            link_labels: std::collections::BTreeMap::new(),
        }
    }

    /// 作成者の列は、保存した値に依らずレコードの作成者で埋まる（列を足す前のレコードも）。
    #[test]
    fn fill_puts_the_owner_into_created_by_columns() {
        let s = schema();
        let mut rs = [
            record("alice", json!({ "title": "a" })),
            record("bob", json!({ "title": "b", "made_by": "alice" })),
        ];
        fill(&s, &mut rs);
        assert_eq!(rs[0].data, json!({ "title": "a", "made_by": "alice" }));
        assert_eq!(rs[1].data["made_by"], json!("bob"));
        // 作成者の列が無ければ触らない。
        let mut plain = s.clone();
        plain.fields.pop();
        let mut rs = [record("alice", json!({ "title": "a" }))];
        fill(&plain, &mut rs);
        assert_eq!(rs[0].data, json!({ "title": "a" }));
    }

    #[test]
    fn write_rejection_names_the_column_by_its_label() {
        let s = schema();
        let e = write_rejected(s.field("made_by").unwrap());
        assert_eq!(e.invalid_code(), Some(InvalidCode::InvalidInput));
        let DataError::InvalidInput(ie) = e else {
            panic!("入力の誤りではない");
        };
        assert_eq!(ie.field_key(), Some("made_by"));
        assert_eq!(
            ie.to_string(),
            "「作成者」には書き込めません。作成者の列には、レコードを追加した人が自動で入ります"
        );
    }

    #[test]
    fn required_and_unique_are_rejected_and_indexed_is_always_on() {
        let mut s = schema();
        normalize(&mut s);
        assert!(s.field("made_by").unwrap().indexed);
        assert!(!s.field("title").unwrap().indexed);
        for set in [
            |f: &mut FieldDef| f.required = true,
            |f: &mut FieldDef| f.unique = true,
        ] {
            let mut f = FieldDef::new("made_by", FieldType::CreatedBy);
            set(&mut f);
            assert!(validate_field(&f).is_err());
        }
        assert!(validate_field(&FieldDef::new("made_by", FieldType::CreatedBy)).is_ok());
    }
}
