//! フィールド識別子 3 層（`id` / `key` / `display_name`）の採番・照合・JSONB キー変換
//! （Phase 15 Task 15.1・data-platform §4）。
//!
//! - **`id`**: テーブル内で採番・不変・再利用しない。保存形の JSONB キー `"f{id}"`。
//!   `0..16` はシステム擬似列に予約し、ユーザー列は [`FIRST_USER_FIELD_ID`] から。上限
//!   [`MAX_FIELD_ID`]（索引の `field_id` が `int2` のため）。次の番号は `data_table.next_field_id`
//!   が持つ（削除済みフィールドの番号も再利用しないため、スキーマ JSONB の外に置く）。
//! - **`key`**: 外部契約の参照名。不変（変更は 422）。
//! - **`display_name`**: 表示名。自由に変えられ、変えてもデータは 1 行も動かない。
//!
//! SQL テキストへ埋め込んでよいのは [`is_json_key`] を通った `"f{id}"` だけ（PIT-21）。

use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value};

use crate::model::{FieldId, FieldType, TableSchema, FIRST_USER_FIELD_ID, MAX_FIELD_ID};
use crate::DataError;

/// 表示名の上限（文字数）。
const MAX_DISPLAY_NAME_CHARS: usize = 128;
/// 説明の上限（文字数）。
const MAX_DESCRIPTION_CHARS: usize = 1_000;

/// 保存形の JSONB キー（`"f{id}"`）。
pub(crate) fn json_key(id: FieldId) -> String {
    format!("f{id}")
}

/// `"f{id}"` 形式（`id` は 10 進・先頭ゼロなし・[`MAX_FIELD_ID`] 以下）か。
///
/// SQL へ埋め込む直前の再検証に使う（スキーマ検証と二重・PIT-21）。
pub(crate) fn is_json_key(s: &str) -> bool {
    let Some(digits) = s.strip_prefix('f') else {
        return false;
    };
    if digits.is_empty() || digits.len() > 5 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    if digits.len() > 1 && digits.starts_with('0') {
        return false;
    }
    digits
        .parse::<u32>()
        .is_ok_and(|n| n <= u32::from(MAX_FIELD_ID))
}

/// 参照名（`key`）が識別子規約（`^[a-z][a-z0-9_]{0,63}$`）を満たすか。
pub(crate) fn is_valid_field_key(key: &str) -> bool {
    let bytes = key.as_bytes();
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

/// 採番済みスキーマの `id` / `display_name` / `description` を検証する。
pub(crate) fn validate_identity(schema: &TableSchema) -> Result<(), DataError> {
    let mut ids = HashSet::new();
    for f in &schema.fields {
        if !(FIRST_USER_FIELD_ID..=MAX_FIELD_ID).contains(&f.id) {
            return Err(DataError::Invalid(format!(
                "フィールド '{}' の id {} が範囲外です（{FIRST_USER_FIELD_ID}〜{MAX_FIELD_ID}）",
                f.key, f.id
            )));
        }
        if !ids.insert(f.id) {
            return Err(DataError::Invalid(format!(
                "フィールド id {} が重複しています",
                f.id
            )));
        }
        let dn = f.display_name.trim();
        if dn.is_empty() || dn.chars().count() > MAX_DISPLAY_NAME_CHARS {
            return Err(DataError::Invalid(format!(
                "フィールド '{}' の display_name は 1〜{MAX_DISPLAY_NAME_CHARS} 文字で指定してください",
                f.key
            )));
        }
        if dn.chars().any(char::is_control) {
            return Err(DataError::Invalid(format!(
                "フィールド '{}' の display_name に制御文字は使えません",
                f.key
            )));
        }
        if let Some(d) = &f.description {
            if d.chars().count() > MAX_DESCRIPTION_CHARS {
                return Err(DataError::Invalid(format!(
                    "フィールド '{}' の description が長すぎます（最大 {MAX_DESCRIPTION_CHARS} 文字）",
                    f.key
                )));
            }
        }
        if f.searchable && f.field_type != FieldType::Text {
            return Err(DataError::Invalid(format!(
                "フィールド '{}': searchable は text 型にのみ指定できます",
                f.key
            )));
        }
    }
    Ok(())
}

/// 入力スキーマへ `id` を採番・照合し、表示名・表示順を正規化する。新しい `next_field_id` を返す。
///
/// - `current = None`（新規作成）: 指定済み `id` は範囲・重複を検証してそのまま使い、
///   未指定（0）は指定済みの最大値の次から宣言順に採番する。
/// - `current = Some((schema, next_field_id))`（改訂）:
///   - `id` 指定 → 既存フィールドに存在し、**`key` が一致**すること（不一致は 422＝key は不変）。
///     既存に無い `id` の指定も 422（ID はサーバだけが採番する・再利用させない）。
///   - `id` 省略 → 既存の同じ `key` があればその `id` を引き継ぐ（v1 互換）。無ければ
///     `next_field_id` から新規採番。
/// - `display_name` が空なら `key`（前後空白は除く）。`order` が全フィールド 0 なら宣言順。
pub(crate) fn assign_field_ids(
    current: Option<(&TableSchema, i32)>,
    next: &mut TableSchema,
) -> Result<i32, DataError> {
    let mut seen_ids: HashSet<FieldId> = HashSet::new();
    let mut counter: i32 = match current {
        Some((_, n)) => n.max(i32::from(FIRST_USER_FIELD_ID)),
        None => next
            .fields
            .iter()
            .map(|f| i32::from(f.id) + 1)
            .max()
            .unwrap_or(0)
            .max(i32::from(FIRST_USER_FIELD_ID)),
    };
    let existing_by_key: HashMap<&str, FieldId> = current
        .map(|(s, _)| s.fields.iter().map(|f| (f.key.as_str(), f.id)).collect())
        .unwrap_or_default();
    let existing_by_id: HashMap<FieldId, &str> = current
        .map(|(s, _)| s.fields.iter().map(|f| (f.id, f.key.as_str())).collect())
        .unwrap_or_default();

    for f in &mut next.fields {
        if f.id != 0 {
            if current.is_some() {
                match existing_by_id.get(&f.id) {
                    Some(k) if *k == f.key => {}
                    Some(k) => {
                        return Err(DataError::Invalid(format!(
                            "フィールド id {} の key は変更できません（'{k}' → '{}'）。表示名は display_name で変更してください",
                            f.id, f.key
                        )));
                    }
                    None => {
                        return Err(DataError::Invalid(format!(
                            "フィールド id {} は存在しません（新しいフィールドは id を省略してください）",
                            f.id
                        )));
                    }
                }
            } else if !(FIRST_USER_FIELD_ID..=MAX_FIELD_ID).contains(&f.id) {
                return Err(DataError::Invalid(format!(
                    "フィールド '{}' の id {} が範囲外です（{FIRST_USER_FIELD_ID}〜{MAX_FIELD_ID}）",
                    f.key, f.id
                )));
            }
        } else if let Some(id) = existing_by_key.get(f.key.as_str()) {
            f.id = *id;
        } else {
            if counter > i32::from(MAX_FIELD_ID) {
                return Err(DataError::Invalid(format!(
                    "フィールド ID が上限（{MAX_FIELD_ID}）に達しました"
                )));
            }
            f.id = FieldId::try_from(counter)
                .map_err(|_| DataError::Invalid("フィールド ID の採番に失敗しました".into()))?;
            counter += 1;
        }
        if !seen_ids.insert(f.id) {
            return Err(DataError::Invalid(format!(
                "フィールド id {} が重複しています",
                f.id
            )));
        }
        let trimmed = f.display_name.trim();
        f.display_name = if trimmed.is_empty() {
            f.key.clone()
        } else {
            trimmed.to_string()
        };
    }
    if next.fields.iter().all(|f| f.order == 0) {
        for (i, f) in next.fields.iter_mut().enumerate() {
            f.order = i32::try_from(i).unwrap_or(i32::MAX);
        }
    }
    let max_assigned = next
        .fields
        .iter()
        .map(|f| i32::from(f.id) + 1)
        .max()
        .unwrap_or(0);
    Ok(counter.max(max_assigned))
}

/// v1 形式（`key_format = 1`）のスキーマを 3 層へ機械的に引き上げる（data-platform §4.3）。
///
/// `id` は宣言順に [`FIRST_USER_FIELD_ID`] から、`display_name = key`、`order` は宣言順。
/// **決定的**（同じ入力から常に同じ ID）なので、移行前の一覧表示と移行後の実データが一致する。
///
/// row_policy が参照する索引の無い列は `indexed = true` に引き上げる（§6.3 の「参照列は indexed 必須」を
/// v1 のテーブルにも満たさせる。台帳の初期化でバックフィルが積まれ、索引経路で読めるようになる）。
pub(crate) fn upgrade_legacy_schema(legacy: &TableSchema) -> Result<(TableSchema, i32), DataError> {
    let mut s = legacy.clone();
    for f in &mut s.fields {
        f.id = 0;
    }
    let next = assign_field_ids(None, &mut s)?;
    crate::policy::validate::promote_policy_fields(&mut s);
    Ok((s, next))
}

/// 外部形（`key` → 値）を保存形（`"f{id}"` → 値）へ写す。未宣言キーは落とす（検証済み前提）。
pub(crate) fn encode_record(schema: &TableSchema, data: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::with_capacity(data.len());
    for (k, v) in data {
        if let Some(f) = schema.field(k) {
            out.insert(f.json_key(), v.clone());
        }
    }
    out
}

/// 保存形（`"f{id}"` → 値）を外部形（`key` → 値）へ写す。
///
/// スキーマの実フィールドだけを拾い、未知のキーは返さない（保存形の内部キーを外へ出さない）。
pub(crate) fn decode_record(schema: &TableSchema, stored: &Value) -> Map<String, Value> {
    let Some(obj) = stored.as_object() else {
        return Map::new();
    };
    let mut out = Map::with_capacity(obj.len());
    for f in &schema.fields {
        if matches!(f.field_type, FieldType::Lookup | FieldType::Computed) {
            continue;
        }
        if let Some(v) = obj.get(&f.json_key()) {
            let v = if f.field_type == FieldType::RecordRef {
                upgrade_link_value(v)
            } else {
                v.clone()
            };
            out.insert(f.key.clone(), v);
        }
    }
    out
}

/// リンク値の読取互換: v1 の `record_ref` の保存値（単一 UUID 文字列）を、現行の保存形（要素 1 件の
/// 配列・正準形の UUID）へ寄せる。配列はそのまま。
///
/// 保存値の移行（[`crate::link_values`]）が終わる前の行も、終わった後の行も同じ形で外へ出す。
/// 移行の途中でも読取・書込・索引の導出が同じ値を見る（書込は常に配列を書く）。
pub(crate) fn upgrade_link_value(v: &Value) -> Value {
    match v {
        Value::String(s) => Value::Array(vec![Value::String(
            uuid::Uuid::parse_str(s).map_or_else(|_| s.clone(), |u| u.to_string()),
        )]),
        other => other.clone(),
    }
}

#[cfg(test)]
#[path = "field_key_tests.rs"]
mod tests;
