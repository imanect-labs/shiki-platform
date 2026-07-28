//! ライブ編集 op の適用（`apply_op`）とセル値 → HTML テーブル変換（issue #352）。
//!
//! `session.rs` から切り出した純ロジック寄りの層（1 ファイル 500 行規約）。
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

/// rows を Calc に貼れる HTML テーブルへ組む。値は**データ**として全て
/// エスケープする（set_cells 経由の HTML/数式注入を構造的に不能化）。
fn rows_to_html_table(rows: &[Vec<CellValue>]) -> String {
    let mut html = String::from("<table>");
    for row in rows {
        html.push_str("<tr>");
        for cell in row {
            match cell {
                CellValue::Text(text) => {
                    let _ = write!(html, "<td>{}</td>", escape_html(text));
                }
                CellValue::Number(n) => {
                    let _ = write!(html, "<td>{n}</td>");
                }
                CellValue::Bool(b) => {
                    let _ = write!(html, "<td>{b}</td>");
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
}
