//! 値 → 索引行の正規化（data-platform §3.4・純関数）。
//!
//! 索引テーブルの `val` は「索引キー」であって値の正本ではない（正本は `data_record.data`）。
//! ここで決めた形がそのまま索引の格納形式・keyset カーソルの中身・parity 検証の基準になる。
//!
//! | 型 | 索引テーブル | 正規化 |
//! |---|---|---|
//! | text / select / date / date_time / user_ref / role_ref / file_ref | `data_index_text` | 保存形の文字列（date・date_time は書込検証が固定幅へ正規化済み） |
//! | multi_select | `data_index_text` | **要素ごとに 1 行**（`ord` = 配列内の位置） |
//! | number | `data_index_num` | 保存形の数値（NumericSpec で正規化済み）を `numeric` へ |
//! | record_ref | `data_link` | 参照先 `(dst_table_int, dst_record)`（record_int への解決は書込側） |
//! | unique 宣言 | `data_unique` | 文字列はそのまま・数値は 10 進正準形・長い値は `<先頭 2,000B>:<sha256>` |
//! | searchable 宣言 | `data_index_search` | 全文（trigram GIN はキー長の制約を受けない） |
//! | 擬似列 owner / created_at / updated_at | `data_index_text` | principal id / UTC 固定幅 ISO-8601 |

use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{IndexTable, MAX_INDEX_KEY_BYTES};
use crate::model::{FieldDef, FieldType};
use crate::DataError;

/// `data_index_text` の 1 行分（`field_id` / `record_int` は呼び出し側が持つ）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TextKey {
    pub ord: i16,
    /// 先頭 [`MAX_INDEX_KEY_BYTES`] バイトのプレフィクス（文字境界で切る）。
    pub val: String,
    /// 切り詰めたか（true なら等値・前方一致は本体で再確認する）。
    pub truncated: bool,
}

/// 1 フィールド × 1 索引テーブル分の索引キー。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FieldKeys {
    Text(Vec<TextKey>),
    /// `numeric` へキャストする 10 進表記（ord, 表記）。
    Num(Vec<(i16, String)>),
    /// 参照先の外部 ID（ord, uuid）。record_int への解決は書込側（DB 参照が要る）。
    Link(Vec<(i16, Uuid)>),
    /// `data_unique.val_norm`。
    Unique(String),
    /// 部分一致の全文（ord, 全文）。
    Search(Vec<(i16, String)>),
}

/// 文字列を `max` バイト以内の**文字境界**で切る（UTF-8 を壊さない）。切ったら `true`。
pub(crate) fn prefix_at_char_boundary(s: &str, max: usize) -> (&str, bool) {
    if s.len() <= max {
        return (s, false);
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    (&s[..end], true)
}

/// `data_index_text` のキー（プレフィクス＋truncated）。
pub(crate) fn text_key(ord: i16, s: &str) -> TextKey {
    let (val, truncated) = prefix_at_char_boundary(s, MAX_INDEX_KEY_BYTES);
    TextKey {
        ord,
        val: val.to_string(),
        truncated,
    }
}

/// `data_unique.val_norm`: 長い値は `<先頭 2,000B（文字境界）>:<sha256 hex>` にして一意性を保つ。
///
/// 短い値は値そのもの。長い値の正規化キーは必ず `:` ＋ 64 桁で終わり、プレフィクス部は
/// 2,000B 以下なので、短い値（≤ 2,000B）と衝突しない（長さで区別できる）。
pub(crate) fn unique_norm(s: &str) -> String {
    let (prefix, truncated) = prefix_at_char_boundary(s, MAX_INDEX_KEY_BYTES);
    if !truncated {
        return s.to_string();
    }
    let digest = Sha256::digest(s.as_bytes());
    let mut hex = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{b:02x}");
    }
    format!("{prefix}:{hex}")
}

/// 擬似列（created_at / updated_at）の固定幅表記。辞書順＝時刻順（date_time の正規化と同じ形）。
pub(crate) fn format_timestamp(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Micros, true)
}

/// フィールド値から 1 索引テーブル分のキーを作る。値が無い（null）なら空（行を持たない）。
///
/// 値は書込検証済み（型・選択肢・要素数・NumericSpec）の保存形を前提とする。前提が崩れていれば
/// （バックフィルで古い不正値に出会った等）`Invalid` を返し、黙って索引から落とさない。
pub(crate) fn field_keys(
    f: &FieldDef,
    table: IndexTable,
    v: &Value,
    max_multi_values: usize,
) -> Result<Option<FieldKeys>, DataError> {
    if v.is_null() {
        return Ok(None);
    }
    let bad = || {
        DataError::Invalid(format!(
            "フィールド '{}' の保存値を索引へ正規化できません",
            f.key
        ))
    };
    let keys = match table {
        IndexTable::Text => FieldKeys::Text(match f.field_type {
            FieldType::MultiSelect => {
                let arr = v.as_array().ok_or_else(bad)?;
                check_multi(f, arr.len(), max_multi_values)?;
                arr.iter()
                    .enumerate()
                    .map(|(i, e)| {
                        let s = e.as_str().ok_or_else(bad)?;
                        Ok(text_key(ord(i)?, s))
                    })
                    .collect::<Result<Vec<_>, DataError>>()?
            }
            _ => vec![text_key(0, v.as_str().ok_or_else(bad)?)],
        }),
        IndexTable::Num => {
            let n = v.as_number().ok_or_else(bad)?;
            FieldKeys::Num(vec![(0, n.to_string())])
        }
        IndexTable::Link => {
            // 保存形は配列（ord = 配列内の位置）。移行前の v1 の単一 UUID 文字列は ord 0 の 1 件（同じキー）。
            let ids: Vec<&Value> = match v {
                Value::Array(a) => a.iter().collect(),
                other => vec![other],
            };
            check_multi(f, ids.len(), max_multi_values)?;
            ids.into_iter()
                .enumerate()
                .map(|(i, e)| {
                    let id = e
                        .as_str()
                        .and_then(|s| Uuid::parse_str(s).ok())
                        .ok_or_else(bad)?;
                    Ok((ord(i)?, id))
                })
                .collect::<Result<Vec<_>, DataError>>()
                .map(FieldKeys::Link)?
        }
        // リンク（record_ref）の unique は単一値の側だけ（v1 の record_ref）。保存形は要素 1 件までの配列
        // （v1 の単一 UUID 文字列も移行が終わるまで受ける）。空配列は値なし。
        IndexTable::Unique if f.field_type == FieldType::RecordRef && v.is_array() => {
            match v.as_array().map(Vec::as_slice) {
                Some([]) => return Ok(None),
                Some([one]) => FieldKeys::Unique(
                    one.as_str()
                        .and_then(|s| Uuid::parse_str(s).ok())
                        .ok_or_else(bad)?
                        .to_string(),
                ),
                _ => return Err(bad()),
            }
        }
        IndexTable::Unique => FieldKeys::Unique(match v {
            Value::Number(n) => crate::numeric::canonical_number(n).ok_or_else(bad)?,
            Value::String(s) => match f.field_type {
                // uuid は大小文字を揃える（同じ参照先を別キーにしない）。
                FieldType::RecordRef | FieldType::FileRef => Uuid::parse_str(s)
                    .map(|u| u.to_string())
                    .map_err(|_| bad())?,
                _ => unique_norm(s),
            },
            _ => return Err(bad()),
        }),
        IndexTable::Search => FieldKeys::Search(vec![(0, v.as_str().ok_or_else(bad)?.to_string())]),
    };
    Ok(Some(keys))
}

fn check_multi(f: &FieldDef, n: usize, max: usize) -> Result<(), DataError> {
    if n > max {
        return Err(DataError::Invalid(format!(
            "'{}' の要素数が多すぎます（最大 {max}）",
            f.key
        )));
    }
    Ok(())
}

fn ord(i: usize) -> Result<i16, DataError> {
    i16::try_from(i).map_err(|_| DataError::Invalid("多値の要素数が上限を超えています".into()))
}

#[cfg(test)]
#[path = "normalize_tests.rs"]
mod tests;
