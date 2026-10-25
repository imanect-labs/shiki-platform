//! 投影テンプレートの検証・世代・本文の生成（Task 15.8）。
//!
//! 索引側（[`super::indexer`]）と post-filter 側（[`super::visible`]）は**同じ関数**で本文を作る
//! （索引に載る文字列と、検索結果として返す文字列の作り方を分けない）。

use std::collections::HashSet;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{RagProjection, MAX_RAG_BODY_FIELDS, MAX_RAG_CHUNK_CHARS};
use crate::field_key::{is_json_key, json_key};
use crate::model::{FieldDef, FieldType, TableSchema};
use crate::DataError;

/// 投影の形式の版（本文の作り方を変えたら上げる＝全テーブルの世代が変わり再構築される）。
const PROJECTION_FORMAT: u32 = 1;

/// 投影できる列の型か（保存値を持ち、閲覧者に依らず同じ値が見える型）。
///
/// 派生列（lookup / computed / rollup / formula）は参照先の 4 階層 authz を閲覧者本人の権限で
/// 読取時に計算する列なので、全閲覧者共通のチャンクへ焼き込めない（PIT-20・PIT-71）。
/// ファイル参照・レコード参照・リンクは値が参照先の id で、本文として意味を持たない。
fn projectable(f: &FieldDef) -> bool {
    f.link.is_none()
        && matches!(
            f.field_type,
            FieldType::Text
                | FieldType::Number
                | FieldType::Date
                | FieldType::DateTime
                | FieldType::Select
                | FieldType::MultiSelect
                | FieldType::UserRef
                | FieldType::RoleRef
        )
}

/// 投影テンプレートを検証する（スキーマ保存時・違反は 400）。
///
/// **フィールドマスク対象列は投影に含められない**（data-platform §11.1・fail-closed）。後から
/// `field_policy` を付ける改訂も同じ検証を通るので、投影列をマスクするには同じ改訂で投影から外す必要がある。
pub(crate) fn validate_projection(schema: &TableSchema) -> Result<(), DataError> {
    let Some(p) = &schema.rag else {
        return Ok(());
    };
    if p.body_fields.len() > MAX_RAG_BODY_FIELDS {
        return Err(crate::schema_invalid(format!(
            "rag.body_fields は {MAX_RAG_BODY_FIELDS} 列までです"
        )));
    }
    let masked: HashSet<&str> = schema
        .field_policy
        .iter()
        .map(|fp| fp.field.as_str())
        .collect();
    let mut seen = HashSet::new();
    for key in std::iter::once(&p.title_field).chain(&p.body_fields) {
        if !seen.insert(key.as_str()) {
            return Err(crate::schema_invalid(format!(
                "rag: 列 '{key}' が投影に重複しています"
            )));
        }
        let f = schema.field(key).ok_or_else(|| {
            crate::schema_invalid(format!("rag: 列 '{key}' が fields にありません"))
        })?;
        if masked.contains(key.as_str()) {
            return Err(crate::schema_invalid(format!(
                "rag: 列 '{key}' はフィールドマスク（field_policy）の対象のため検索の投影に含められません"
            )));
        }
        if !projectable(f) {
            return Err(crate::schema_invalid(format!(
                "rag: 列 '{key}' の型は検索の投影に含められません（派生列・参照列・リンクは不可）"
            )));
        }
    }
    Ok(())
}

/// 投影する列（タイトルが先頭・保存形キー `"f{id}"` と表示名）。
pub(crate) fn projected(schema: &TableSchema) -> Option<Vec<(String, String)>> {
    let p = schema.rag.as_ref()?;
    std::iter::once(&p.title_field)
        .chain(&p.body_fields)
        .map(|k| {
            let f = schema.field(k)?;
            let key = json_key(f.id);
            is_json_key(&key).then(|| (key, f.display_name.clone()))
        })
        .collect()
}

/// 投影列の参照名（`key`）。実行時のマスク再確認に使う。
pub(crate) fn projected_field_keys(p: &RagProjection) -> impl Iterator<Item = &str> {
    std::iter::once(p.title_field.as_str()).chain(p.body_fields.iter().map(String::as_str))
}

/// 索引の世代（オプトインしていなければ `None`）。
///
/// **チャンクの本文または安全性を変えるもの全部**から作る: 形式の版・投影列（id・表示名）・
/// `field_policy` 全体。`field_policy` は投影列以外への追加・変更でも世代を変える（保守的に倒す。
/// 改訂は稀で、再構築のコストより「マスクの変更が索引に残らない」ことを取る）。行述語（`row_policy`）
/// とテーブル ReBAC は世代に入れない（post-filter が毎回評価するので索引に焼き込まれていない）。
pub(crate) fn generation(schema: &TableSchema) -> Option<String> {
    let cols = projected(schema)?;
    let material = json!({
        "v": PROJECTION_FORMAT,
        "cols": cols,
        "field_policy": schema.field_policy,
    });
    let digest = Sha256::digest(material.to_string().as_bytes());
    Some(
        digest[..16]
            .iter()
            .fold(String::with_capacity(32), |mut out, b| {
                use std::fmt::Write as _;
                let _ = write!(out, "{b:02x}");
                out
            }),
    )
}

/// 投影した 1 件（タイトルと本文）。
pub(crate) struct Rendered {
    pub title: String,
    pub body: String,
}

/// 値を本文の文字列にする（null・欠落・オブジェクトは載せない）。
fn value_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().filter_map(value_text).collect();
            (!parts.is_empty()).then(|| parts.join(", "))
        }
        _ => None,
    }
}

/// 文字数で切り詰める（文字境界）。
fn truncate_chars(s: &mut String, max: usize) {
    if let Some((i, _)) = s.char_indices().nth(max) {
        s.truncate(i);
    }
}

/// 投影列の値（保存形キーのオブジェクト）から 1 チャンクを作る。
///
/// 本文は「表示名: 値」を改行で連結し、タイトルと合わせて [`MAX_RAG_CHUNK_CHARS`] 文字で切り詰める。
pub(crate) fn render(cols: &[(String, String)], values: &Value) -> Rendered {
    let mut it = cols.iter();
    let title = it
        .next()
        .and_then(|(k, _)| values.get(k))
        .and_then(value_text)
        .unwrap_or_default();
    let mut body = it
        .filter_map(|(k, label)| {
            values
                .get(k)
                .and_then(value_text)
                .map(|v| format!("{label}: {v}"))
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut title = title;
    truncate_chars(&mut title, MAX_RAG_CHUNK_CHARS);
    truncate_chars(
        &mut body,
        MAX_RAG_CHUNK_CHARS.saturating_sub(title.chars().count()),
    );
    Rendered { title, body }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::model::FieldPolicy;
    use crate::policy::ast::PolicyExpr;

    fn schema(rag: Option<RagProjection>, masked: &[&str]) -> TableSchema {
        let mut s = TableSchema {
            fields: vec![
                FieldDef::new("title", FieldType::Text),
                FieldDef::new("body", FieldType::Text),
                FieldDef::new("salary", FieldType::Number),
                FieldDef::new("tags", FieldType::MultiSelect),
                FieldDef::new("total", FieldType::Formula),
            ],
            status_field: None,
            row_policy: None,
            field_policy: masked
                .iter()
                .map(|f| FieldPolicy {
                    field: (*f).into(),
                    readable_by: PolicyExpr::HasRole {
                        role: "hr".into(),
                        subtree: false,
                    },
                })
                .collect(),
            aggregate_min_rows: None,
            fsm_ref: None,
            rag,
        };
        crate::field_key::assign_field_ids(None, &mut s).unwrap();
        s
    }

    #[allow(clippy::unnecessary_wraps)] // schema(rag: Option<_>) へそのまま渡す。
    fn proj(title: &str, body: &[&str]) -> Option<RagProjection> {
        Some(RagProjection {
            title_field: title.into(),
            body_fields: body.iter().map(|s| (*s).into()).collect(),
        })
    }

    #[test]
    fn masked_column_cannot_be_projected() {
        let s = schema(proj("title", &["body", "salary"]), &["salary"]);
        let e = validate_projection(&s).unwrap_err();
        assert!(
            e.invalid_code() == Some(crate::InvalidCode::SchemaInvalid)
                && e.invalid_message()
                    .is_some_and(|m| m.contains("field_policy")),
            "{e:?}"
        );
        // タイトル側でも同じ。
        let s = schema(proj("salary", &[]), &["salary"]);
        assert!(validate_projection(&s).is_err());
        // マスクされていなければ通る。
        let s = schema(proj("title", &["body", "salary"]), &["tags"]);
        validate_projection(&s).unwrap();
    }

    #[test]
    fn derived_unknown_and_duplicate_columns_are_rejected() {
        for p in [
            proj("title", &["total"]),
            proj("nope", &[]),
            proj("title", &["title"]),
            proj("title", &["body", "body"]),
        ] {
            assert!(
                validate_projection(&schema(p.clone(), &[])).is_err(),
                "{p:?}"
            );
        }
        let many: Vec<&str> = std::iter::repeat_n("body", MAX_RAG_BODY_FIELDS + 1).collect();
        assert!(validate_projection(&schema(proj("title", &many), &[])).is_err());
    }

    #[test]
    fn generation_changes_with_field_policy_and_projection_only() {
        let base = schema(proj("title", &["body"]), &[]);
        let g0 = generation(&base).unwrap();
        assert_eq!(generation(&base).unwrap(), g0, "決定的");
        // 投影列以外への field_policy の追加でも世代は変わる（保守的に倒す）。
        assert_ne!(
            generation(&schema(proj("title", &["body"]), &["salary"])).unwrap(),
            g0
        );
        assert_ne!(
            generation(&schema(proj("title", &["body", "tags"]), &[])).unwrap(),
            g0
        );
        // 行述語は世代に入らない（post-filter が毎回評価する）。
        let mut with_rp = base.clone();
        with_rp.row_policy = Some(crate::policy::RowPolicy {
            read: PolicyExpr::IsOwner,
            write: None,
        });
        assert_eq!(generation(&with_rp).unwrap(), g0);
        assert!(generation(&schema(None, &[])).is_none());
    }

    #[test]
    fn render_joins_labels_and_truncates() {
        let s = schema(proj("title", &["body", "tags", "salary"]), &[]);
        let cols = projected(&s).unwrap();
        let r = render(
            &cols,
            &json!({"f16": "出張申請", "f17": "大阪", "f19": ["a", "b"], "f18": null}),
        );
        assert_eq!(r.title, "出張申請");
        assert_eq!(r.body, "body: 大阪\ntags: a, b");
        let long = "あ".repeat(MAX_RAG_CHUNK_CHARS + 10);
        let r = render(&cols, &json!({"f16": "t", "f17": long}));
        assert_eq!(
            r.title.chars().count() + r.body.chars().count(),
            MAX_RAG_CHUNK_CHARS
        );
    }
}
