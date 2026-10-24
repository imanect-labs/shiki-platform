//! レコード書込のサーバ検証（型・必須・選択肢・参照整合・Task 9.2）。
//!
//! 参照の存在検証は [`RefResolver`] に委譲する（user/role=directory、file=StorageService。
//! record_ref は DB アクセスが要るため [`crate::record`] が担う）。

use async_trait::async_trait;
use authz::AuthContext;
use chrono::{DateTime, Datelike, NaiveDate, SecondsFormat, Timelike, Utc};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::model::{FieldDef, FieldType, TableSchema};
use crate::{DataError, InvalidCode};

/// text フィールドの値の上限長（防御的上限）。索引テーブルには先頭
/// [`crate::index::MAX_INDEX_KEY_BYTES`] だけを載せるので、この長さでも btree キー上限で落ちない。
pub(crate) const MAX_TEXT_LEN: usize = 10_000;

/// 参照整合の解決器（user/role/file の存在・可読チェック）。
///
/// api 層が directory / StorageService を束ねて実装する（data crate は
/// ストレージ実体へ依存しない。チョークポイント経由の検証を注入で受ける）。
#[async_trait]
pub trait RefResolver: Send + Sync {
    /// ユーザー（principal id）がテナント内に存在するか。
    async fn user_exists(&self, ctx: &AuthContext, user_id: &str) -> Result<bool, String>;
    /// ロール（部署）がテナント内に存在するか。
    async fn role_exists(&self, ctx: &AuthContext, role_id: &str) -> Result<bool, String>;
    /// ファイルが存在し、**呼出ユーザーが読めるか**（StorageService の authz 込み）。
    async fn file_readable(&self, ctx: &AuthContext, file_id: Uuid) -> Result<bool, String>;
}

/// 書込ペイロードをスキーマで検証し、**正規化済みの保存形**を返す。
///
/// - 未宣言フィールド・派生フィールド（lookup/computed）への書込は拒否。
/// - `full` = create（required の欠落を拒否）。update は merge 後の完全形を渡す。
/// - datetime は UTC 固定幅 ISO-8601 へ正規化する（辞書順＝時刻順）。
/// - number は `numeric_spec` に従って正規化する（data-platform §4.2）。
/// - 多値（multi_select）の要素数は `max_multi_values` まで（PIT-72・超過は 422）。
pub(crate) async fn validate_record_data(
    ctx: &AuthContext,
    schema: &TableSchema,
    data: &Value,
    resolver: &dyn RefResolver,
    max_multi_values: usize,
) -> Result<Map<String, Value>, DataError> {
    let obj = data.as_object().ok_or_else(|| {
        DataError::input_detail(
            InvalidCode::InvalidInput,
            None,
            "data は JSON オブジェクトである必要があります",
        )
    })?;
    reject_nul_in_object(obj)?;

    // 未宣言・派生フィールドの拒否（閉じたスキーマ）。列名は書き手が名指ししたもの。
    for key in obj.keys() {
        let Some(f) = schema.field(key) else {
            return Err(field_err(
                InvalidCode::InvalidInput,
                key,
                format!("フィールド '{key}' はスキーマに存在しません"),
            ));
        };
        if f.field_type.is_derived() {
            return Err(field_err(
                InvalidCode::InvalidInput,
                key,
                format!("フィールド '{key}' は派生フィールドのため書込できません"),
            ));
        }
        if f.is_reverse_link() {
            return Err(field_err(InvalidCode::InvalidInput, key, format!(
                    "フィールド '{key}' は対称列（逆リンク）のため書込できません。リンク元の列を更新してください"
                ),
            ));
        }
    }

    let mut out = Map::with_capacity(obj.len());
    for f in &schema.fields {
        let value = obj.get(&f.key);
        match value {
            None | Some(Value::Null) => {
                if f.required {
                    return Err(required_missing(&f.key));
                }
                // 省略・null は「値なし」（保存形にキーを残さない）。
            }
            Some(v) => {
                let normalized =
                    validate_field_value(ctx, f, v, resolver, max_multi_values).await?;
                // リンクの空配列は「値なし」（null と同じ・required を空配列で満たさせない）。
                if f.field_type == FieldType::RecordRef
                    && normalized.as_array().is_some_and(Vec::is_empty)
                {
                    if f.required {
                        return Err(required_missing(&f.key));
                    }
                    continue;
                }
                out.insert(f.key.clone(), normalized);
            }
        }
    }
    Ok(out)
}

/// 列の参照名つきの入力の誤り（文言は書き手が名指しした列名と宣言済みの上限だけから作る）。
fn field_err(code: InvalidCode, key: &str, message: String) -> DataError {
    DataError::input_detail(code, Some(key), message)
}

fn required_missing(key: &str) -> DataError {
    field_err(
        InvalidCode::RequiredMissing,
        key,
        format!("必須フィールド '{key}' がありません"),
    )
}

fn not_an_option(key: &str) -> DataError {
    field_err(
        InvalidCode::InvalidOption,
        key,
        format!("'{key}' の値は選択肢にありません"),
    )
}

fn out_of_range(key: &str, why: &str) -> DataError {
    field_err(InvalidCode::ValueOutOfRange, key, format!("'{key}': {why}"))
}

/// NUL 文字（U+0000）の拒否（Postgres の text / jsonb は NUL を保存できない・検証 D-8）。
///
/// 入口で 400 にする（以前はデータベースの誤りがそのまま 500 になった）。列名は書き手が名指ししたもの。
pub(crate) fn reject_nul_in_object(obj: &Map<String, Value>) -> Result<(), DataError> {
    for (k, v) in obj {
        if k.contains('\0') || contains_nul(v) {
            return Err(nul_error(
                Some(k).filter(|k| !k.contains('\0')).map(String::as_str),
            ));
        }
    }
    Ok(())
}

/// 値の中（入れ子の配列・オブジェクトのキーと文字列）に NUL があるか。
pub(crate) fn contains_nul(v: &Value) -> bool {
    match v {
        Value::String(s) => s.contains('\0'),
        Value::Array(a) => a.iter().any(contains_nul),
        Value::Object(m) => m.iter().any(|(k, v)| k.contains('\0') || contains_nul(v)),
        _ => false,
    }
}

/// NUL 文字の入力誤り（構造化 400 `invalid_input`）。
pub(crate) fn nul_error(key: Option<&str>) -> DataError {
    DataError::input_detail(
        InvalidCode::InvalidInput,
        key,
        "NUL 文字（U+0000）は使えません",
    )
}

fn type_mismatch(name: &str, want: &str) -> DataError {
    field_err(
        InvalidCode::ValueTypeMismatch,
        name,
        format!("フィールド '{name}' は {want} である必要があります"),
    )
}

/// date の値（`YYYY-MM-DD`・0001〜9999 年）をゼロ詰めの正準形へ正規化する。
fn validate_date(name: &str, v: &Value) -> Result<Value, DataError> {
    let s = v
        .as_str()
        .ok_or_else(|| type_mismatch(name, "YYYY-MM-DD 文字列"))?;
    let d = NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map_err(|_| type_mismatch(name, "YYYY-MM-DD 文字列"))?;
    // 固定幅（辞書順＝日付順）を保てる 0001〜9999 年だけ（紀元前・5 桁の年を拒否・検証 D-12）。
    if !(1..=9999).contains(&d.year()) {
        return Err(out_of_range(
            name,
            "日付は 0001-01-01〜9999-12-31 の範囲で指定してください",
        ));
    }
    // chrono はゼロ詰めなし（2026-7-5）も受理するため、**ゼロ詰めの正準形へ正規化**
    // して保存する（辞書順＝日付順と等値フィルタの一致性を保証する）。
    Ok(Value::String(d.format("%Y-%m-%d").to_string()))
}

/// date_time の値（RFC3339・タイムゾーン必須）を UTC 固定幅へ正規化する（閏秒・範囲外の年は拒否）。
fn validate_date_time(name: &str, v: &Value) -> Result<Value, DataError> {
    let s = v
        .as_str()
        .ok_or_else(|| type_mismatch(name, "RFC3339 文字列"))?;
    let dt = DateTime::parse_from_rfc3339(s).map_err(|_| type_mismatch(name, "RFC3339 文字列"))?;
    // UTC 固定幅（マイクロ秒・Z 終端）へ正規化: 辞書順＝時刻順を保証する。
    let utc: DateTime<Utc> = dt.with_timezone(&Utc);
    // 閏秒（:60）は保存形の固定幅・時刻順を崩すので受け付けない。年も 0001〜9999 だけ（検証 D-12）。
    if utc.nanosecond() >= 1_000_000_000 {
        return Err(out_of_range(name, "閏秒（:60）は指定できません"));
    }
    if !(1..=9999).contains(&utc.year()) {
        return Err(out_of_range(
            name,
            "日時は 0001〜9999 年の範囲で指定してください",
        ));
    }
    Ok(Value::String(
        utc.to_rfc3339_opts(SecondsFormat::Micros, true),
    ))
}

/// 単一フィールド値の型検証＋正規化。
async fn validate_field_value(
    ctx: &AuthContext,
    f: &FieldDef,
    v: &Value,
    resolver: &dyn RefResolver,
    max_multi_values: usize,
) -> Result<Value, DataError> {
    let name = &f.key;
    let type_err = |want: &str| type_mismatch(name, want);
    match f.field_type {
        FieldType::Text => {
            let s = v.as_str().ok_or_else(|| type_err("文字列"))?;
            // 上限は文字数（Unicode スカラー値の数・UI の「字」と同じ数え方・検証 A-3）。
            if s.chars().count() > MAX_TEXT_LEN {
                return Err(field_err(
                    InvalidCode::ValueTooLong,
                    name,
                    format!("'{name}' が長すぎます（最大 {MAX_TEXT_LEN} 文字）"),
                ));
            }
            Ok(v.clone())
        }
        FieldType::Number => {
            let n = v.as_f64().ok_or_else(|| type_err("数値"))?;
            if !n.is_finite() {
                return Err(type_err("有限の数値"));
            }
            crate::numeric::normalize_number(f, v)
        }
        FieldType::Date => validate_date(name, v),
        FieldType::DateTime => validate_date_time(name, v),
        FieldType::Select => {
            let s = v.as_str().ok_or_else(|| type_err("文字列"))?;
            if !f.options.iter().any(|o| o == s) {
                return Err(not_an_option(name));
            }
            Ok(v.clone())
        }
        FieldType::MultiSelect => {
            let arr = v.as_array().ok_or_else(|| type_err("文字列配列"))?;
            if arr.len() > max_multi_values {
                return Err(field_err(
                    InvalidCode::TooManyValues,
                    name,
                    format!("'{name}' の選択数が多すぎます（最大 {max_multi_values}）"),
                ));
            }
            let mut seen = std::collections::HashSet::new();
            for item in arr {
                let s = item.as_str().ok_or_else(|| type_err("文字列配列"))?;
                if !f.options.iter().any(|o| o == s) {
                    return Err(not_an_option(name));
                }
                if !seen.insert(s) {
                    return Err(field_err(
                        InvalidCode::InvalidOption,
                        name,
                        format!("'{name}' に重複する選択があります"),
                    ));
                }
            }
            Ok(v.clone())
        }
        FieldType::UserRef => {
            let s = require_nonempty_str(v, name)?;
            let ok = resolver
                .user_exists(ctx, s)
                .await
                .map_err(DataError::Internal)?;
            if !ok {
                return Err(field_err(
                    InvalidCode::InvalidOption,
                    name,
                    format!("'{name}' のユーザーが見つかりません"),
                ));
            }
            Ok(v.clone())
        }
        FieldType::RoleRef => {
            let s = require_nonempty_str(v, name)?;
            let ok = resolver
                .role_exists(ctx, s)
                .await
                .map_err(DataError::Internal)?;
            if !ok {
                return Err(field_err(
                    InvalidCode::InvalidOption,
                    name,
                    format!("'{name}' のロールが見つかりません"),
                ));
            }
            Ok(v.clone())
        }
        FieldType::FileRef => {
            let id = require_uuid_str(v, name)?;
            let ok = resolver
                .file_readable(ctx, id)
                .await
                .map_err(DataError::Internal)?;
            if !ok {
                // 存在しない/読めないは同一応答（ファイルの存在オラクルを作らない）。
                return Err(field_err(
                    InvalidCode::InvalidOption,
                    name,
                    format!("'{name}' のファイルが見つかりません"),
                ));
            }
            Ok(v.clone())
        }
        FieldType::RecordRef => validate_link_value(f, v, max_multi_values),
        FieldType::Lookup | FieldType::Computed | FieldType::Rollup | FieldType::Formula => {
            Err(field_err(
                InvalidCode::InvalidInput,
                name,
                format!("フィールド '{name}' は派生フィールドのため書込できません"),
            ))
        }
    }
}

/// record_ref の値（形式のみ。存在検証は record 層が DB で行う）。
fn validate_link_value(
    f: &FieldDef,
    v: &Value,
    max_multi_values: usize,
) -> Result<Value, DataError> {
    let name = &f.key;
    let Some(link) = &f.link else {
        // v1 の record_ref（リンク定義なし＝many_to_one・片方向）: 保存形は常に要素 1 件までの配列
        // （data-platform §7.1）。互換のため単一 UUID 文字列の入力も受け、配列へ正規化して書く。
        let arr = match v {
            Value::String(_) => vec![v.clone()],
            Value::Array(a) => a.clone(),
            _ => {
                return Err(field_err(InvalidCode::ValueTypeMismatch, name, format!(
                        "フィールド '{name}' は UUID 文字列（または要素 1 件までの UUID 配列）である必要があります"
                    ),
                ))
            }
        };
        return canonical_link_array(name, &arr, 1);
    };
    // リンク定義つき: 値は常に UUID の配列（重複なし・単一値の側は 1 件まで・PIT-72 の上限）。
    let arr = v.as_array().ok_or_else(|| {
        field_err(
            InvalidCode::ValueTypeMismatch,
            name,
            format!("フィールド '{name}' は UUID 文字列の配列 である必要があります"),
        )
    })?;
    let max = if link.cardinality.src_single() {
        1
    } else {
        max_multi_values
    };
    canonical_link_array(name, arr, max)
}

/// リンク値の配列を正準形（小文字ハイフン区切りの UUID・重複なし・`max` 件まで）にする。
fn canonical_link_array(name: &str, arr: &[Value], max: usize) -> Result<Value, DataError> {
    if arr.len() > max {
        return Err(field_err(
            InvalidCode::TooManyValues,
            name,
            format!("'{name}' のリンク先が多すぎます（最大 {max}）"),
        ));
    }
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let id = require_uuid_str(item, name)?;
        if !seen.insert(id) {
            return Err(field_err(
                InvalidCode::InvalidOption,
                name,
                format!("'{name}' に重複するリンク先があります"),
            ));
        }
        out.push(Value::String(id.to_string()));
    }
    Ok(Value::Array(out))
}

fn require_nonempty_str<'v>(v: &'v Value, name: &str) -> Result<&'v str, DataError> {
    let s = v.as_str().ok_or_else(|| {
        field_err(
            InvalidCode::ValueTypeMismatch,
            name,
            format!("'{name}' は文字列である必要があります"),
        )
    })?;
    if s.is_empty() || s.len() > 256 {
        return Err(field_err(
            InvalidCode::InvalidInput,
            name,
            format!("'{name}' の id が不正です"),
        ));
    }
    Ok(s)
}

fn require_uuid_str(v: &Value, name: &str) -> Result<Uuid, DataError> {
    let s = v.as_str().ok_or_else(|| {
        field_err(
            InvalidCode::ValueTypeMismatch,
            name,
            format!("'{name}' は UUID 文字列である必要があります"),
        )
    })?;
    Uuid::parse_str(s).map_err(|_| {
        field_err(
            InvalidCode::ValueTypeMismatch,
            name,
            format!("'{name}' は UUID 文字列である必要があります"),
        )
    })
}

#[cfg(test)]
#[path = "validate_tests.rs"]
mod tests;
