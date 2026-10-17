//! 接続済みのセッションに対する Excel の読み書き（#725）。
//!
//! セッションの開閉・認可・保存は呼び出し側（`OfficeEngine`）が持つ。ここは
//! 「エンジンへ JSON を渡し、結果を LLM 向けのテキストにする」だけなので、偽 WOPI と実機の
//! Collabora に対して直接結合テストできる。

use std::fmt::Write as _;

use serde_json::{json, Value};

use super::normalize::{mm100_to_chars, Normalized};
use super::view::{self, error_label, text_width_px};
use crate::engine::a1::{cell_name, quote_sheet, Area};
use crate::engine::{call, EngineError};
use crate::live::CoolWsClient;

/// 1 回の読み取りで返すセル数の既定上限。
pub const DEFAULT_MAX_CELLS: u32 = 1_500;

/// 読み戻しで確かめるセル数の上限（範囲ごと・全体）。
const READBACK_CELLS_PER_AREA: u32 = 400;
const READBACK_AREAS: usize = 4;

/// ブックの概要（シート・使用範囲・グラフ・名前）＋作業中シートの先頭部分。
pub async fn read_overview(
    client: &mut CoolWsClient,
    file_name: &str,
) -> Result<String, EngineError> {
    let overview = call(client, json!({"action": "calc.overview"})).await?;
    let mut text = view::overview_text(file_name, &overview);
    // 作業中のシートの左上（30 行 × 15 列）を添え、最初の 1 往復で表の形が分かるようにする。
    let active = overview["active"].as_str().map(str::to_string);
    if let Some(sheet) = active {
        let preview = call(
            client,
            json!({"action": "calc.read_range", "sheet": sheet, "area": [0, 0, 14, 29],
                   "max_cells": 450, "formats": true, "merges": true, "sheet_features": false}),
        )
        .await?;
        let _ = write!(text, "\n{}", view::range_text(&preview));
    }
    Ok(text)
}

/// 範囲を読む（値・数式・書式・結合・列幅。`sheet_features` でシート全体の付帯情報も）。
pub async fn read_range(
    client: &mut CoolWsClient,
    area: &Area,
    default_sheet: Option<&str>,
    max_cells: u32,
    sheet_features: bool,
) -> Result<String, EngineError> {
    let sheet = area.sheet.as_deref().or(default_sheet);
    let v = call(
        client,
        json!({"action": "calc.read_range", "sheet": sheet, "area": area.coords(),
               "max_cells": max_cells, "formats": true, "merges": true, "sheet_features": sheet_features}),
    )
    .await?;
    Ok(view::range_text(&v))
}

/// 適用の結果。
#[derive(Debug, Clone)]
pub struct ApplyOutcome {
    /// LLM へ返す観測テキスト。
    pub text: String,
    /// 文書を変えたか（false なら丸ごと取り消し済み）。
    pub applied: bool,
}

/// 正規化済みの操作をまとめて適用し、書いた範囲を読み戻して確かめる。
pub async fn apply(
    client: &mut CoolWsClient,
    label: &str,
    normalized: &Normalized,
) -> Result<ApplyOutcome, EngineError> {
    let result = call(
        client,
        json!({"action": "calc.apply", "label": format!("Shiki AI: {label}"), "ops": normalized.ops}),
    )
    .await;
    let results = match result {
        Ok(v) => v,
        Err(EngineError::Rejected {
            code,
            message,
            stage,
            op_index,
        }) => {
            let which = op_index
                .map(|i| format!("{} 件目の操作", i + 1))
                .unwrap_or_default();
            let mut text = format!(
                "適用できませんでした。途中までの変更もすべて取り消したので、文書は変わっていません。\n原因: {which}（{stage}）: {message}"
            );
            if code == "sheet_not_found" {
                let _ = write!(text, "\nシート名は office.read の概要で確かめてください。");
            }
            return Ok(ApplyOutcome {
                text,
                applied: false,
            });
        }
        Err(other) => return Err(other),
    };
    let count = results["results"].as_array().map_or(0, Vec::len);
    let mut text =
        format!("{count} 件の操作を適用しました（人は 1 回の「元に戻す」で取り消せます）。");
    if results["results"]
        .as_array()
        .is_some_and(|r| r.iter().any(|x| x["outside_undo"] == Value::Bool(true)))
    {
        let _ = write!(
            text,
            "\n※ ウィンドウ枠の固定は表示の設定なので「元に戻す」の対象外です。"
        );
    }
    // 書いた範囲を読み戻す（計算結果・エラー・列幅不足を観測させる）。
    let mut warnings = Vec::new();
    for t in normalized.touched.iter().take(READBACK_AREAS) {
        let v = call(
            client,
            json!({"action": "calc.read_range", "sheet": t.sheet, "area": t.area,
                   "max_cells": READBACK_CELLS_PER_AREA, "formats": false, "merges": false, "sheet_features": false}),
        )
        .await?;
        let _ = write!(text, "\n\n現在の値:\n{}", view::range_text(&v));
        lint_range(&v, &mut warnings);
    }
    if !warnings.is_empty() {
        let _ = write!(text, "\n確認が必要な点:");
        for w in &warnings {
            let _ = write!(text, "\n- {w}");
        }
    }
    Ok(ApplyOutcome {
        text,
        applied: true,
    })
}

/// 読み戻した範囲の決定的な検査（数式エラー・数値の列幅不足）。
fn lint_range(v: &Value, out: &mut Vec<String>) {
    let sheet = v["sheet"].as_str().unwrap_or("");
    let widths: std::collections::HashMap<u64, u64> = v["columns"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| Some((c[0].as_u64()?, c[1].as_u64()?)))
        .collect();
    for c in v["cells"].as_array().into_iter().flatten() {
        let (Some(col), Some(row)) = (c[0].as_u64(), c[1].as_u64()) else {
            continue;
        };
        let (col32, row32) = (
            u32::try_from(col).unwrap_or(0),
            u32::try_from(row).unwrap_or(0),
        );
        let at = format!("{}!{}", quote_sheet(sheet), cell_name(col32, row32));
        if let Some(code) = c.get(5).and_then(Value::as_i64) {
            let (label, why) = error_label(code);
            out.push(format!("{at} が {label}（{why}）"));
            continue;
        }
        let kind = c[2].as_str().unwrap_or("");
        let display = c[4].as_str().unwrap_or("");
        let numeric = kind == "n" || (kind == "f" && c[3].is_string() && looks_numeric(display));
        if !numeric {
            continue;
        }
        if let Some(w) = widths.get(&col) {
            let col_px = f64::from(u32::try_from(*w).unwrap_or(0)) / 2540.0 * 96.0;
            if text_width_px(display) > col_px {
                out.push(format!(
                    "{at} の「{display}」が列幅（{} 文字）に収まらず ### と表示される恐れがあります（column_width で広げる）",
                    mm100_to_chars(u32::try_from(*w).unwrap_or(0))
                ));
            }
        }
    }
}

fn looks_numeric(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| {
            c.is_ascii_digit()
                || matches!(
                    c,
                    ',' | '.' | '-' | '%' | '¥' | '$' | '(' | ')' | '/' | ':' | ' '
                )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lint_flags_errors_and_narrow_numeric_columns() {
        let v = json!({
            "sheet": "売上",
            "columns": [[0, 1693], [1, 900]],
            "cells": [
                [0, 0, "n", 1_234_567_890.0, "1,234,567,890"],
                [1, 0, "n", 5, "5"],
                [1, 1, "f", "=1/0", "#DIV/0!", 532],
                [0, 1, "s", null, "とても長い見出しの文字列ですがこれは文字なので対象外"]
            ]
        });
        let mut w = Vec::new();
        lint_range(&v, &mut w);
        assert_eq!(w.len(), 2, "{w:?}");
        assert!(w[0].contains("売上!A1 の「1,234,567,890」"), "{w:?}");
        assert!(w[1].contains("売上!B2 が #DIV/0!"), "{w:?}");
    }
}
