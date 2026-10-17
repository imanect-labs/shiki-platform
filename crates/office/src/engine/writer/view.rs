//! Word の読み取り結果（writer_read.js の生データ）を LLM 向けに整形する（#725）。
//!
//! 概要はブロックの一覧（ID・種類・スタイル・冒頭の文）。詳細は、そのまま replace に渡せる HTML
//! （段落は data-style とインラインの書式、表はセルの塗り・結合）で返す。

use std::fmt::Write as _;

use serde_json::Value;

fn s(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}

fn hex(n: i64) -> Option<String> {
    (n >= 0).then(|| format!("#{:06X}", n & 0x00FF_FFFF))
}

fn mm(v: &Value) -> String {
    let x = v.as_f64().unwrap_or(0.0) / 100.0;
    format!("{x:.0}")
}

fn align_label(a: i64) -> Option<&'static str> {
    // com.sun.star.style.ParagraphAdjust: LEFT=0, RIGHT=1, BLOCK=2, CENTER=3（STRETCH=4）。
    match a {
        1 => Some("右揃え"),
        2 => Some("両端揃え"),
        3 => Some("中央揃え"),
        _ => None,
    }
}

fn kind_label(b: &Value) -> String {
    let props = &b["props"];
    let style = s(&props["ParaStyleName"]);
    let mut label = style.to_string();
    if props["NumberingIsNumber"] == Value::Bool(true) {
        let lvl = props["NumberingLevel"].as_i64().unwrap_or(0) + 1;
        let mark = s(&props["ListLabelString"]);
        label = if mark.is_empty() || mark == "•" {
            format!("{style}・箇条書き{lvl}")
        } else {
            format!("{style}・番号 {mark}")
        };
    }
    if let Some(a) = align_label(props["ParaAdjust"].as_i64().unwrap_or(0)) {
        label.push('・');
        label.push_str(a);
    }
    if s(&props["BreakType"]).starts_with("PAGE") {
        label.push_str("・改ページ");
    }
    if b["changes"] == Value::Bool(true) {
        label.push_str("・変更履歴あり");
    }
    label
}

/// スタイルの見た目（"14pt 太字 #2E74B5"）。
fn style_look(look: &Value) -> Option<String> {
    let size = look["size"].as_f64()?;
    let mut out = format!("{size}pt");
    if look["bold"] == Value::Bool(true) {
        out.push_str(" 太字");
    }
    if let Some(c) = hex(look["color"].as_i64().unwrap_or(-1)) {
        out.push(' ');
        out.push_str(&c);
    }
    Some(out)
}

/// 用紙の名前（1/100 mm の幅・高さから）。知らない大きさは寸法で書く。
fn paper_name(width: u64, height: u64) -> String {
    let name = match (width.min(height), width.max(height)) {
        (20900..=21100, 29600..=29800) => "A4",
        (29600..=29800, 41900..=42100) => "A3",
        (18100..=18300, 25600..=25800) => "B5",
        (25600..=25800, 36300..=36500) => "B4",
        (14700..=14900, 20900..=21100) => "A5",
        (21500..=21700, 27800..=28000) => "Letter",
        (21500..=21700, 35500..=35700) => "Legal",
        _ => return format!("用紙 {}×{}mm ", (width + 50) / 100, (height + 50) / 100),
    };
    name.to_string()
}

/// 文書の概要（writer.overview の結果）。
pub fn overview_text(file_name: &str, ov: &Value) -> String {
    let mut out = String::new();
    let page = &ov["page"];
    let (width, height) = (
        page["Width"].as_u64().unwrap_or(0),
        page["Height"].as_u64().unwrap_or(0),
    );
    let paper = paper_name(width, height);
    let orient = if width > height { "横" } else { "縦" };
    let _ = write!(
        out,
        "文書{}: ブロック {} 個・{paper}{orient}・余白 上{} 右{} 下{} 左{} mm",
        if file_name.is_empty() {
            String::new()
        } else {
            format!("「{file_name}」")
        },
        ov["count"],
        mm(&page["TopMargin"]),
        mm(&page["RightMargin"]),
        mm(&page["BottomMargin"]),
        mm(&page["LeftMargin"]),
    );
    if page["HeaderIsOn"] == Value::Bool(true) {
        let _ = write!(out, "・ヘッダー「{}」", s(&page["header"]).trim());
    }
    if page["FooterIsOn"] == Value::Bool(true) {
        let _ = write!(out, "・フッター「{}」", s(&page["footer"]).trim());
    }
    if ov["indexes"].as_u64().unwrap_or(0) > 0 {
        out.push_str("・目次あり");
    }
    if ov["tracking"] == Value::Bool(true) {
        out.push_str("・変更履歴を記録中");
    }
    if let Some(n) = ov["redlines"].as_u64().filter(|n| *n > 0) {
        let _ = write!(
            out,
            "・未承認の変更 {n} 件（read_blocks で <ins>/<del> として見える）"
        );
    }
    if let Some(n) = ov["pages"].as_u64().filter(|n| *n > 0) {
        let _ = write!(out, "・{n} ページ");
    }
    out.push('\n');
    if let Some(styles) = ov["styles"].as_array() {
        let list: Vec<String> = styles
            .iter()
            .filter_map(|pair| {
                let pair = pair.as_array()?;
                let (name, display) = (s(&pair[0]), s(&pair[1]));
                let mut notes = Vec::new();
                if name != display {
                    notes.push(display.to_string());
                }
                if let Some(look) = pair.get(2).and_then(style_look) {
                    notes.push(look);
                }
                Some(if notes.is_empty() {
                    name.to_string()
                } else {
                    format!("{name}（{}）", notes.join("・"))
                })
            })
            .collect();
        let _ = writeln!(
            out,
            "使える段落スタイル（data-style / style に書く名前）: {}",
            list.join("、")
        );
    }
    let blocks = ov["blocks"].as_array().cloned().unwrap_or_default();
    let from = ov["from"].as_u64().unwrap_or(0);
    let _ = writeln!(
        out,
        "ブロック（{}〜{}）:",
        from,
        from + blocks.len().saturating_sub(1) as u64
    );
    let mut i = 0;
    while i < blocks.len() {
        let b = &blocks[i];
        // 目次の中の段落は 1 行にまとめる。
        if b["index"] == Value::Bool(true) {
            let mut j = i;
            while j + 1 < blocks.len() && blocks[j + 1]["index"] == Value::Bool(true) {
                j += 1;
            }
            let _ = writeln!(
                out,
                "[{}] 目次（{} 段落・update_toc で更新）",
                s(&b["id"]),
                j - i + 1
            );
            i = j + 1;
            continue;
        }
        if s(&b["t"]) == "table" {
            let _ = writeln!(
                out,
                "[{}] 表 {}行×{}列「{}」",
                s(&b["id"]),
                b["rows"],
                b["cols"],
                s(&b["text"]).chars().take(60).collect::<String>()
            );
        } else {
            let text = s(&b["text"]);
            let shown = if text.is_empty() {
                "（空の段落）".to_string()
            } else {
                format!("「{text}」")
            };
            let _ = writeln!(out, "[{}] {}{}", s(&b["id"]), kind_label(b), shown);
        }
        i += 1;
    }
    let total = ov["count"].as_u64().unwrap_or(0);
    if from + (blocks.len() as u64) < total {
        let _ = writeln!(
            out,
            "…（続きは range: \"from:{}\" で読む）",
            from + blocks.len() as u64
        );
    }
    out
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// ランを HTML の断片にする（太字・斜体・下線・取り消し・色・大きさ・蛍光ペン・リンク）。
fn run_html(r: &Value) -> String {
    let p = &r["props"];
    let mut text = escape(s(&r["text"]));
    if text.is_empty() {
        return text;
    }
    let mut css = Vec::new();
    if let Some(c) = hex(p["CharColor"].as_i64().unwrap_or(-1)) {
        css.push(format!("color: {c}"));
    }
    if let Some(c) = hex(p["CharBackColor"].as_i64().unwrap_or(-1)) {
        css.push(format!("background-color: {c}"));
    }
    if let Some(sz) = p["CharHeight"].as_f64() {
        css.push(format!("font-size: {sz}pt"));
    }
    let font = s(&p["CharFontNameAsian"]);
    let font = if font.is_empty() {
        s(&p["CharFontName"])
    } else {
        font
    };
    if !font.is_empty() {
        css.push(format!("font-family: '{}'", escape(font)));
    }
    if !css.is_empty() {
        text = format!("<span style=\"{}\">{text}</span>", css.join("; "));
    }
    if p["CharWeight"].as_f64().unwrap_or(100.0) > 100.0 {
        text = format!("<b>{text}</b>");
    }
    if s(&p["CharPosture"]) == "ITALIC" {
        text = format!("<i>{text}</i>");
    }
    if p["CharUnderline"].as_i64().unwrap_or(0) != 0 {
        text = format!("<u>{text}</u>");
    }
    if p["CharStrikeout"].as_i64().unwrap_or(0) != 0 {
        text = format!("<s>{text}</s>");
    }
    match p["CharEscapement"].as_i64().unwrap_or(0) {
        e if e > 0 => text = format!("<sup>{text}</sup>"),
        e if e < 0 => text = format!("<sub>{text}</sub>"),
        _ => {}
    }
    let url = s(&p["HyperLinkURL"]);
    if !url.is_empty() {
        text = format!("<a href=\"{}\">{text}</a>", escape(url));
    }
    // 未承認の変更履歴（挿入・削除）。
    match s(&r["redline"]) {
        "Insert" => text = format!("<ins>{text}</ins>"),
        "Delete" => text = format!("<del>{text}</del>"),
        _ => {}
    }
    text
}

fn paragraph_html(p: &Value) -> String {
    let props = &p["props"];
    let mut css = Vec::new();
    if let Some(a) = align_label(props["ParaAdjust"].as_i64().unwrap_or(0)) {
        let v = match a {
            "右揃え" => "right",
            "中央揃え" => "center",
            _ => "justify",
        };
        css.push(format!("text-align: {v}"));
    }
    let indent = props["ParaFirstLineIndent"].as_f64().unwrap_or(0.0);
    if indent.abs() > 0.5 {
        css.push(format!("text-indent: {:.1}mm", indent / 100.0));
    }
    let left = props["ParaLeftMargin"].as_f64().unwrap_or(0.0);
    if left > 0.5 {
        css.push(format!("margin-left: {:.1}mm", left / 100.0));
    }
    // 段落に直接付けた間隔（スタイル由来の値は出さない）。1/100 mm → pt。
    let direct = &p["direct"];
    for (key, name) in [
        ("ParaTopMargin", "margin-top"),
        ("ParaBottomMargin", "margin-bottom"),
    ] {
        if let Some(v) = direct[key].as_f64() {
            css.push(format!("{name}: {:.1}pt", v / 100.0 * 72.0 / 25.4));
        }
    }
    match (
        direct["LineSpacingMode"].as_i64(),
        direct["LineSpacingHeight"].as_f64(),
    ) {
        (Some(0), Some(h)) if (h - 100.0).abs() > 0.5 => {
            css.push(format!("line-height: {}", h / 100.0));
        }
        (Some(3), Some(h)) => {
            css.push(format!("line-height: {:.1}pt", h / 100.0 * 72.0 / 25.4));
        }
        _ => {}
    }
    let style_attr = if css.is_empty() {
        String::new()
    } else {
        format!(" style=\"{}\"", css.join("; "))
    };
    let inner: String = p["runs"]
        .as_array()
        .map(|rs| rs.iter().map(run_html).collect())
        .unwrap_or_default();
    format!(
        "<p data-style=\"{}\"{style_attr}>{inner}</p>",
        escape(s(&props["ParaStyleName"]))
    )
}

/// セル名（`B3`）→ (列, 行) 0 始まり。結合の印の付いた名前（`A2.1.1`）は None。
fn cell_pos(name: &str) -> Option<(u32, u32)> {
    let letters = name.chars().take_while(char::is_ascii_alphabetic).count();
    let (c, r) = name.split_at(letters);
    let col = crate::engine::a1::parse_col(c)?;
    let row: u32 = r.parse().ok()?;
    Some((col, row.checked_sub(1)?))
}

fn table_html(t: &Value) -> String {
    let mut out = String::from("<table>");
    let cells = t["cells"].as_array().cloned().unwrap_or_default();
    let mut current_row = None;
    for c in &cells {
        let Some((_, row)) = cell_pos(s(&c["name"])) else {
            continue;
        };
        if current_row != Some(row) {
            if current_row.is_some() {
                out.push_str("</tr>");
            }
            out.push_str("<tr>");
            current_row = Some(row);
        }
        let mut attrs = String::new();
        if let Some(bg) = hex(c["bg"].as_i64().unwrap_or(-1)) {
            let _ = write!(attrs, " style=\"background-color: {bg}\"");
        }
        let span = c["row_span"].as_i64().unwrap_or(1);
        if span > 1 {
            let _ = write!(attrs, " rowspan=\"{span}\"");
        }
        let inner: Vec<String> = c["paras"]
            .as_array()
            .map(|ps| ps.iter().map(paragraph_html).collect())
            .unwrap_or_default();
        let _ = write!(
            out,
            "<td data-cell=\"{}\"{attrs}>{}</td>",
            s(&c["name"]),
            inner.join("")
        );
    }
    if current_row.is_some() {
        out.push_str("</tr>");
    }
    out.push_str("</table>");
    out
}

/// ブロックの詳細（writer.read_blocks の結果）。
pub fn blocks_text(v: &Value) -> String {
    let mut out = String::from(
        "各ブロックの HTML（replace にそのまま渡せる形。表の data-cell はセル名で、行ごとのセル数の違いは結合を表す）:\n",
    );
    for b in v["blocks"].as_array().into_iter().flatten() {
        let html = if s(&b["t"]) == "table" {
            table_html(b)
        } else {
            paragraph_html(b)
        };
        let _ = writeln!(out, "[{}] {html}", s(&b["id"]));
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use serde_json::json;

    #[test]
    fn overview_lists_blocks_and_collapses_toc() {
        let v = json!({
            "count": 5, "from": 0, "indexes": 1, "tracking": false,
            "page": {"Width": 21000, "Height": 29700, "TopMargin": 2500, "RightMargin": 2000, "BottomMargin": 2500, "LeftMargin": 3000, "FooterIsOn": true, "footer": "- 1 -"},
            "styles": [["Standard", "標準"], ["Heading 1", "見出し 1"]],
            "blocks": [
                {"id": "b0-aaaa", "t": "p", "index": true, "text": "目次", "props": {"ParaStyleName": "Contents Heading"}},
                {"id": "b1-bbbb", "t": "p", "index": true, "text": "1 概要\t1", "props": {"ParaStyleName": "Contents 1"}},
                {"id": "b2-cccc", "t": "p", "text": "報告書", "props": {"ParaStyleName": "Heading 1", "ParaAdjust": 3}},
                {"id": "b3-dddd", "t": "table", "rows": 2, "cols": 3, "text": "a|b|c"},
                {"id": "b4-eeee", "t": "p", "text": "項目", "props": {"ParaStyleName": "List Bullet", "NumberingIsNumber": true, "NumberingLevel": 0, "ListLabelString": "•"}}
            ]
        });
        let text = overview_text("報告書.docx", &v);
        assert!(
            text.contains(
                "ブロック 5 個・A4縦・余白 上25 右20 下25 左30 mm・フッター「- 1 -」・目次あり"
            ),
            "{text}"
        );
        assert!(text.contains("Heading 1（見出し 1）"), "{text}");
        assert!(
            text.contains("[b0-aaaa] 目次（2 段落・update_toc で更新）"),
            "{text}"
        );
        assert!(
            text.contains("[b2-cccc] Heading 1・中央揃え「報告書」"),
            "{text}"
        );
        assert!(text.contains("[b3-dddd] 表 2行×3列「a|b|c」"), "{text}");
        assert!(
            text.contains("[b4-eeee] List Bullet・箇条書き1「項目」"),
            "{text}"
        );
    }

    #[test]
    fn blocks_render_as_reusable_html() {
        let v = json!({"blocks": [
            {"id": "b2-cccc", "t": "p", "props": {"ParaStyleName": "Standard", "ParaAdjust": 1},
             "runs": [{"text": "売上は ", "props": {"CharWeight": 100}}, {"text": "24億円", "props": {"CharWeight": 150, "CharColor": 12_582_912}}, {"kind": "Bookmark"}]},
            {"id": "b3-dddd", "t": "table", "cells": [
                {"name": "A1", "bg": 2_046_052, "paras": [{"props": {"ParaStyleName": "Standard"}, "runs": [{"text": "部門", "props": {}}]}]},
                {"name": "A2", "bg": -1, "row_span": 2, "paras": []}
            ]}
        ]});
        let text = blocks_text(&v);
        assert!(text.contains("[b2-cccc] <p data-style=\"Standard\" style=\"text-align: right\">売上は <b><span style=\"color: #C00000\">24億円</span></b></p>"), "{text}");
        assert!(text.contains("<tr><td data-cell=\"A1\" style=\"background-color: #1F3864\"><p data-style=\"Standard\">部門</p></td></tr><tr><td data-cell=\"A2\" rowspan=\"2\"></td></tr>"), "{text}");
    }
}
