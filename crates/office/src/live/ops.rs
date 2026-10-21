//! ライブ編集 op の適用（`apply_op`）とセル値 → HTML テーブル変換（issue #352）。
//!
//! `session.rs` から切り出した純ロジック寄りの層（1 ファイル行数規約）。
//! 「セッション継続不能」は `Err`、対象不一致などの安全な不発は `applied=false`＋warning。

use std::fmt::Write as _;

use super::client::{CoolWsClient, SearchOutcome};
use super::error::LiveError;
use super::protocol::is_cell_ref;
use super::session::{CellValue, LiveOp, LiveOpResult};

/// paste に使う HTML の mimetype。
const HTML_MIME: &str = "text/html;charset=utf-8";

/// 1 op をセッションへ適用する。Err は「セッションが継続不能」（abort）を意味し、
/// 対象不一致などの安全な不発は `applied=false`＋warning で返す。
pub(super) async fn apply_op(
    client: &mut CoolWsClient,
    content_type: &str,
    op: &LiveOp,
) -> Result<LiveOpResult, LiveError> {
    let label = op.label();
    let unapplied = |warning: &str| LiveOpResult {
        op: label,
        applied: false,
        warning: Some(warning.to_string()),
    };
    match op {
        LiveOp::ReplaceText { find, html } => {
            if find.trim().is_empty() {
                return Ok(unapplied("find が空です"));
            }
            if client.execute_search(find).await? == SearchOutcome::NotFound {
                return Ok(unapplied("検索文字列が見つかりません"));
            }
            // 自 view の選択を照合してから置換する（同文異所の誤置換を防ぐ最終ゲート）。
            let selection = client.selection_text().await?;
            if normalize_for_match(&selection) != normalize_for_match(find) {
                return Ok(unapplied(
                    "選択の照合が一致しませんでした（検索文字列を文書内で一意な文字列にしてください）",
                ));
            }
            let pasted = client.paste(HTML_MIME, html.as_bytes()).await?;
            Ok(LiveOpResult {
                op: label,
                applied: pasted,
                warning: (!pasted).then(|| "core が HTML を貼り付けられませんでした".to_string()),
            })
        }
        LiveOp::AppendHtml { html } => {
            if !content_type.contains("wordprocessingml") {
                return Ok(unapplied("append_html は docx のみ対応です"));
            }
            client.go_to_end_of_doc().await?;
            let pasted = client.paste(HTML_MIME, html.as_bytes()).await?;
            Ok(LiveOpResult {
                op: label,
                applied: pasted,
                warning: (!pasted).then(|| "core が HTML を貼り付けられませんでした".to_string()),
            })
        }
        LiveOp::SetCells { anchor, rows } => {
            if !content_type.contains("spreadsheetml") {
                return Ok(unapplied("set_cells は xlsx のみ対応です"));
            }
            if !is_cell_ref(anchor) {
                return Ok(unapplied("anchor が不正です（例: \"A1\"・\"Sheet2.B3\"）"));
            }
            if rows.is_empty() || rows.iter().all(Vec::is_empty) {
                return Ok(unapplied("rows が空です"));
            }
            client.go_to_cell(anchor).await?;
            let table = rows_to_html_table(rows);
            let pasted = client.paste(HTML_MIME, table.as_bytes()).await?;
            if pasted {
                autofit_columns(client, anchor, rows).await;
            }
            Ok(LiveOpResult {
                op: label,
                applied: pasted,
                warning: (!pasted).then(|| "core がテーブルを貼り付けられませんでした".to_string()),
            })
        }
    }
}

/// 照合用の正規化（空白run→1 個・前後 trim）。LibreOffice は選択テキストの改行・
/// 空白を厳密には保存しないため、意味を変えない範囲で吸収する。
fn normalize_for_match(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 貼り込んだ矩形の列幅を内容に合わせる（#385）。
///
/// 既定の列幅のままだと桁数の多い数値が `2.4E+07`（General）や `###`（書式付き）へ
/// 縮退して読めない。**失敗しても値は入っている**ので op は不発扱いにせず warning
/// ログだけ残す（paste は非冪等・PIT-47 なので再送もしない）。
async fn autofit_columns(client: &mut CoolWsClient, anchor: &str, rows: &[Vec<CellValue>]) {
    let Some(range) = pasted_range(anchor, rows) else {
        tracing::warn!(anchor, "貼り込み範囲を特定できず列幅調整を省略しました");
        return;
    };
    // 調整対象は**選択中の列**なので、貼り込んだ矩形を選び直してから実行する。
    let adjusted = async {
        client.go_to_cell(&range).await?;
        client.set_optimal_column_width().await
    }
    .await;
    if let Err(e) = adjusted {
        tracing::warn!(error = %e, range, "列幅の自動調整に失敗（値の書き込みは成功）");
    }
}

/// `anchor` 起点に `rows` を貼ったときに埋まるセル範囲（例 `B2` × 3 行 4 列 → `B2:E4`）。
///
/// シート接頭辞は保存する。範囲アンカー（`A1:C4`）は開始セルだけを起点に使う
/// （paste は左上から実データの大きさで広がるため）。Calc の上限を超える場合は
/// `None`（列幅調整を省略するだけで、貼り込み自体には影響しない）。
fn pasted_range(anchor: &str, rows: &[Vec<CellValue>]) -> Option<String> {
    let (sheet, cell_part) = match anchor.rsplit_once('.') {
        Some((sheet, rest)) => (Some(sheet), rest),
        None => (None, anchor),
    };
    let start = cell_part
        .split_once(':')
        .map_or(cell_part, |(from, _)| from);
    let (start_col, start_row) = split_cell(start)?;
    let width = u32::try_from(rows.iter().map(Vec::len).max()?).ok()?;
    let height = u32::try_from(rows.len()).ok()?;
    if width == 0 || height == 0 {
        return None;
    }
    let end_col = column_name(column_index(start_col)?.checked_add(width - 1)?)?;
    let end_row = start_row
        .checked_add(height - 1)
        .filter(|r| *r <= MAX_ROW)?;
    Some(match sheet {
        Some(sheet) => format!("{sheet}.{start}:{end_col}{end_row}"),
        None => format!("{start}:{end_col}{end_row}"),
    })
}

/// Calc の列数上限（XFD）。
const MAX_COLUMN: u32 = 16_384;
/// Calc の行数上限。
const MAX_ROW: u32 = 1_048_576;

/// `B2` → (`B`, 2)。列名が無い・行番号が数値でない場合は `None`。
fn split_cell(cell: &str) -> Option<(&str, u32)> {
    let col_len = cell.chars().take_while(char::is_ascii_uppercase).count();
    if col_len == 0 {
        return None;
    }
    let (col, row) = cell.split_at(col_len);
    row.parse().ok().filter(|r| *r >= 1).map(|row| (col, row))
}

/// 列名（`A`・`AA`）→ 1 始まりの列番号。
fn column_index(name: &str) -> Option<u32> {
    let mut index: u32 = 0;
    for ch in name.chars() {
        if !ch.is_ascii_uppercase() {
            return None;
        }
        let digit = u32::from(ch as u8 - b'A') + 1;
        index = index.checked_mul(26)?.checked_add(digit)?;
    }
    (1..=MAX_COLUMN).contains(&index).then_some(index)
}

/// 1 始まりの列番号 → 列名（26 進 bijective 表記）。
fn column_name(mut index: u32) -> Option<String> {
    if !(1..=MAX_COLUMN).contains(&index) {
        return None;
    }
    let mut name = Vec::new();
    while index > 0 {
        name.push(b'A' + u8::try_from((index - 1) % 26).ok()?);
        index = (index - 1) / 26;
    }
    name.reverse();
    String::from_utf8(name).ok()
}

/// 桁区切り書式を与える下限。これ未満は General のまま扱う（0.75・52.7 のような
/// 比率・小数に `#,##0` を被せて丸めてしまわないため）。
const GROUPING_MIN: f64 = 10_000.0;

/// `sdnum` の第 1 フィールド（書式コードを解釈するロケール）。1041 = ja-JP で
/// `load` の `lang=ja` と揃える（桁区切り `,`・小数点 `.`）。
const SDNUM_LANG: u16 = 1041;

/// 桁の多い数値に与える表示書式。小さい値・非有限値には付けない（General のまま）。
fn grouping_format(n: f64) -> Option<&'static str> {
    if !n.is_finite() || n.abs() < GROUPING_MIN {
        return None;
    }
    Some(if n.fract() == 0.0 {
        "#,##0"
    } else {
        "#,##0.00"
    })
}

/// 先頭行を見出し行とみなすか。
///
/// 「全セルが文字列の先頭行」＋「2 行目以降に数値/真偽が 1 つでもある」を集計表の
/// signature とする。誤検知しても先頭行が太字・中央寄せになるだけで値は変わらない。
fn has_header_row(rows: &[Vec<CellValue>]) -> bool {
    let Some((head, body)) = rows.split_first() else {
        return false;
    };
    !head.is_empty()
        && head.iter().all(|c| matches!(c, CellValue::Text(_)))
        && body
            .iter()
            .flatten()
            .any(|c| !matches!(c, CellValue::Text(_)))
}

/// rows を Calc に貼れる HTML テーブルへ組む。値は**データ**として全て
/// エスケープする（set_cells 経由の HTML/数式注入を構造的に不能化）。
///
/// 桁の多い数値には `sdval`/`sdnum`（LibreOffice の HTML クリップボード拡張属性）で
/// 桁区切り書式を添える。値そのものは `sdval` が持つので、表示だけが変わり後続の
/// 数式計算には影響しない（#385）。見出し行は `<th>`＝太字・中央寄せになる。
fn rows_to_html_table(rows: &[Vec<CellValue>]) -> String {
    let header = has_header_row(rows);
    let mut html = String::from("<table>");
    for (index, row) in rows.iter().enumerate() {
        let tag = if header && index == 0 { "th" } else { "td" };
        html.push_str("<tr>");
        for cell in row {
            match cell {
                CellValue::Text(text) => {
                    let _ = write!(html, "<{tag}>{}</{tag}>", escape_html(text));
                }
                // f64 の Display は指数表記を使わないため、そのまま属性値に載せられる。
                CellValue::Number(n) => match grouping_format(*n) {
                    Some(format) => {
                        let _ = write!(
                            html,
                            "<{tag} sdval=\"{n}\" sdnum=\"{SDNUM_LANG};0;{format}\">{n}</{tag}>"
                        );
                    }
                    None => {
                        let _ = write!(html, "<{tag}>{n}</{tag}>");
                    }
                },
                CellValue::Bool(b) => {
                    let _ = write!(html, "<{tag}>{b}</{tag}>");
                }
            }
        }
        html.push_str("</tr>");
    }
    html.push_str("</table>");
    html
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn table_escapes_cell_values() {
        let html = rows_to_html_table(&[
            vec![CellValue::Text("<b>x</b>&y".into()), CellValue::Number(2.5)],
            vec![CellValue::Bool(true)],
        ]);
        assert_eq!(
            html,
            "<table><tr><td>&lt;b&gt;x&lt;/b&gt;&amp;y</td><td>2.5</td></tr><tr><td>true</td></tr></table>"
        );
    }

    #[test]
    fn normalize_collapses_whitespace() {
        assert_eq!(normalize_for_match("  a\n b\tc "), "a b c");
        assert_ne!(normalize_for_match("ab"), normalize_for_match("a b"));
    }

    /// #385: 桁の多い数値は `sdval`/`sdnum` で桁区切り書式が付き、見出し行は `<th>`。
    #[test]
    fn table_formats_large_numbers_and_header_row() {
        let html = rows_to_html_table(&[
            vec![
                CellValue::Text("四半期".into()),
                CellValue::Text("受注額合計".into()),
                CellValue::Text("勝率".into()),
            ],
            vec![
                CellValue::Text("2026Q1".into()),
                CellValue::Number(24_000_000.0),
                CellValue::Number(0.75),
            ],
            vec![
                CellValue::Text("2026Q2".into()),
                CellValue::Number(28_500_000.5),
                CellValue::Number(52.7),
            ],
        ]);
        assert_eq!(
            html,
            "<table>\
             <tr><th>四半期</th><th>受注額合計</th><th>勝率</th></tr>\
             <tr><td>2026Q1</td><td sdval=\"24000000\" sdnum=\"1041;0;#,##0\">24000000</td><td>0.75</td></tr>\
             <tr><td>2026Q2</td><td sdval=\"28500000.5\" sdnum=\"1041;0;#,##0.00\">28500000.5</td><td>52.7</td></tr>\
             </table>"
        );
    }

    /// 見出しの誤検知を避ける: 全行が文字列・単一行・先頭行に数値がある表は `<td>` のまま。
    #[test]
    fn header_row_requires_text_head_and_numeric_body() {
        let text = CellValue::Text("a".into());
        let number = CellValue::Number(1.0);
        assert!(!has_header_row(&[]));
        assert!(!has_header_row(&[vec![text.clone()]]));
        assert!(!has_header_row(&[vec![], vec![number.clone()]]));
        assert!(!has_header_row(&[
            vec![text.clone(), number.clone()],
            vec![text.clone(), number.clone()],
        ]));
        assert!(!has_header_row(&[
            vec![text.clone()],
            vec![CellValue::Text("b".into())],
        ]));
        assert!(has_header_row(&[vec![text], vec![number]]));
    }

    #[test]
    fn grouping_format_applies_only_to_wide_numbers() {
        assert_eq!(grouping_format(24_000_000.0), Some("#,##0"));
        assert_eq!(grouping_format(-10_000.0), Some("#,##0"));
        assert_eq!(grouping_format(12_345.6), Some("#,##0.00"));
        assert_eq!(grouping_format(9_999.0), None);
        assert_eq!(grouping_format(0.75), None);
        assert_eq!(grouping_format(f64::NAN), None);
        assert_eq!(grouping_format(f64::INFINITY), None);
    }

    #[test]
    fn pasted_range_covers_the_written_rectangle() {
        let row = |n: usize| vec![CellValue::Number(1.0); n];
        assert_eq!(
            pasted_range("B2", &[row(4), row(4), row(4)]).as_deref(),
            Some("B2:E4")
        );
        // 行ごとに列数が違う場合は最大幅で覆う。
        assert_eq!(
            pasted_range("A1", &[row(1), row(3)]).as_deref(),
            Some("A1:C2")
        );
        assert_eq!(
            pasted_range("Sheet2.Z10", &[row(2)]).as_deref(),
            Some("Sheet2.Z10:AA10")
        );
        // 範囲アンカーは開始セルだけを起点に使う。
        assert_eq!(pasted_range("A1:C4", &[row(2)]).as_deref(), Some("A1:B1"));
        // 上限超え・空 rows は None（列幅調整を省略するだけ）。
        assert_eq!(pasted_range("XFD1", &[row(2)]), None);
        assert_eq!(pasted_range("A1048576", &[row(1), row(1)]), None);
        assert_eq!(pasted_range("A1", &[]), None);
        assert_eq!(pasted_range("A1", &[vec![]]), None);
    }

    #[test]
    fn column_names_round_trip() {
        for (index, name) in [
            (1, "A"),
            (26, "Z"),
            (27, "AA"),
            (702, "ZZ"),
            (16_384, "XFD"),
        ] {
            assert_eq!(column_index(name), Some(index));
            assert_eq!(column_name(index).as_deref(), Some(name));
        }
        assert_eq!(column_index("XFE"), None);
        assert_eq!(column_index("a"), None);
        assert_eq!(column_name(0), None);
        assert_eq!(split_cell("AB12"), Some(("AB", 12)));
        assert_eq!(split_cell("12"), None);
        assert_eq!(split_cell("A0"), None);
    }
}
