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
    let font = s(&look["font"]);
    if !font.is_empty() {
        out.push(' ');
        out.push_str(font);
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
/// 最後のページへの少しだけのはみ出し（例 1 枚に収めたつもりの案内状の 2 行が 2 ページ目へ出た）。
///
/// 一覧が文書の全ブロックを含むときだけ判定する（途中から読んだ一覧では最後のページが分からない）。
pub fn page_warnings(ov: &Value) -> Vec<String> {
    let pages = ov["pages"].as_u64().unwrap_or(0);
    let blocks = ov["blocks"].as_array().cloned().unwrap_or_default();
    let whole = ov["from"].as_u64() == Some(0) && ov["count"].as_u64() == Some(blocks.len() as u64);
    if pages < 2 || !whole {
        return Vec::new();
    }
    let mut out = Vec::new();
    // 前のページの最後のブロックが表か長い段落なら、そのページへ続いている（ブロックは始まるページしか
    // 持たないので、続きのあるページは「ほぼ空」「少しだけはみ出し」に見えてしまう）。
    let continues_into = |p: u64| {
        blocks
            .iter()
            .rfind(|b| b["page"].as_u64() == Some(p - 1))
            .is_some_and(|b| s(&b["t"]) == "table" || s(&b["text"]).ends_with('…'))
    };
    // 途中のほぼ空のページ（見出しだけ・空の段落だけ）。表紙の余白の取りすぎや、改ページの重なりで起きる。
    // 1 ページ目（表紙の題だけ、はあり得る）と、前のページから続くページは除く。
    for p in 2..pages {
        if continues_into(p) {
            continue;
        }
        let on: Vec<&Value> = blocks
            .iter()
            .filter(|b| b["page"].as_u64() == Some(p))
            .collect();
        let filled: Vec<&&Value> = on
            .iter()
            .filter(|b| s(&b["t"]) == "table" || !s(&b["text"]).trim().is_empty())
            .collect();
        let nearly_empty = !on.is_empty()
            && filled.len() <= 1
            && filled
                .iter()
                .all(|b| s(&b["t"]) != "table" && s(&b["text"]).chars().count() <= 20);
        if nearly_empty {
            let what = filled.first().map_or_else(
                || "空の段落".to_string(),
                |b| format!("「{}」", s(&b["text"])),
            );
            out.push(format!(
                "{p} ページ目には {what} だけしかありません（ほぼ空のページ）。余白（margin-top）・改ページ\
                 （page_break・page-break-before）の重なりを見直してください"
            ));
        }
    }
    let on_last: Vec<&Value> = blocks
        .iter()
        .filter(|b| b["page"].as_u64() == Some(pages))
        .collect();
    let short = on_last.len() <= 3
        && on_last
            .iter()
            .all(|b| s(&b["t"]) != "table" && s(&b["text"]).chars().count() <= 120);
    if on_last.is_empty() || !short || continues_into(pages) {
        return out;
    }
    let ids: Vec<String> = on_last.iter().map(|b| s(&b["id"]).to_string()).collect();
    out.push(format!(
        "最後の {pages} ページ目には {} ブロック（{}）だけがはみ出しています。{} ページに収めたいなら、\
         余白（page_setup）・段落の間隔（margin）・文字の大きさを詰めるか、内容を減らしてください",
        on_last.len(),
        ids.join("、"),
        pages - 1
    ));
    out
}

/// 一覧の頭（用紙・余白・ヘッダー/フッター・ページ数・使えるスタイル）。
fn overview_header(out: &mut String, file_name: &str, ov: &Value) {
    let page = &ov["page"];
    let (width, height) = (
        page["Width"].as_u64().unwrap_or(0),
        page["Height"].as_u64().unwrap_or(0),
    );
    let paper = paper_name(width, height);
    let orient = if width > height { "横" } else { "縦" };
    let _ = write!(
        *out,
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
        let _ = write!(*out, "・ヘッダー「{}」", s(&page["header"]).trim());
    }
    if page["FooterIsOn"] == Value::Bool(true) {
        let _ = write!(*out, "・フッター「{}」", s(&page["footer"]).trim());
    }
    if page["title_page"] == Value::Bool(true) {
        out.push_str("・表紙あり（1 ページ目はヘッダー・フッターなし）");
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
        let _ = write!(*out, "・{n} ページ");
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
            *out,
            "使える段落スタイル（data-style / style に書く名前）: {}",
            list.join("、")
        );
    }
}

pub fn overview_text(file_name: &str, ov: &Value) -> String {
    let mut out = String::new();
    overview_header(&mut out, file_name, ov);
    let blocks = ov["blocks"].as_array().cloned().unwrap_or_default();
    let from = ov["from"].as_u64().unwrap_or(0);
    let _ = writeln!(
        out,
        "ブロック（{}〜{}）:",
        from,
        from + blocks.len().saturating_sub(1) as u64
    );
    let many_pages = ov["pages"].as_u64().unwrap_or(0) > 1;
    let mut last_page = None;
    let mut i = 0;
    while i < blocks.len() {
        let b = &blocks[i];
        // 複数ページなら、ページが変わるところに区切りを入れる（どこで改ページされたかを見せる）。
        if let Some(p) = b["page"].as_u64().filter(|_| many_pages) {
            if last_page != Some(p) {
                let _ = writeln!(out, "── {p} ページ目 ──");
                last_page = Some(p);
            }
        }
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
    let list = props["NumberingIsNumber"] == Value::Bool(true);
    let indent = props["ParaFirstLineIndent"].as_f64().unwrap_or(0.0);
    // 箇条書きの字下げは番号の設定から来るので出さない（<ul> / <ol> として返す）。
    if indent.abs() > 0.5 && !list {
        css.push(format!("text-indent: {:.1}mm", indent / 100.0));
    }
    let left = props["ParaLeftMargin"].as_f64().unwrap_or(0.0);
    if left > 0.5 && !list {
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
    let style_name = escape(s(&props["ParaStyleName"]));
    if list {
        return list_item_html(props, &style_name, &style_attr, &inner);
    }
    format!("<p data-style=\"{style_name}\"{style_attr}>{inner}</p>")
}

/// 番号の文字列の最後の数字の並び（多段の番号 `1.2.` はその段の `2`・`(3)` は `3`）。
fn last_number(label: &str) -> String {
    let rev: Vec<char> = label
        .chars()
        .rev()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(char::is_ascii_digit)
        .collect();
    rev.into_iter().rev().collect()
}

/// 箇条書き・番号付きの段落を、その段落だけの <ul> / <ol> にする（入れ子はレベルの数だけ重ねる）。
fn list_item_html(props: &Value, style: &str, style_attr: &str, inner: &str) -> String {
    let label = s(&props["ListLabelString"]).trim();
    let digits = last_number(label);
    let (open, close) = if digits.is_empty() {
        ("<ul>".to_string(), "</ul>")
    } else {
        (format!("<ol start=\"{digits}\">"), "</ol>")
    };
    let level =
        usize::try_from(props["NumberingLevel"].as_i64().unwrap_or(0).clamp(0, 8)).unwrap_or(0);
    let nest_open = open.repeat(level + 1);
    let nest_close = close.repeat(level + 1);
    format!("{nest_open}<li data-style=\"{style}\"{style_attr}>{inner}</li>{nest_close}")
}

/// 1/100 mm → pt（小数 1 桁）。
fn pt_of(v: f64) -> String {
    format!("{:.1}pt", v * 72.0 / 2540.0)
}

/// セルの罫線 [上, 下, 左, 右]（各 [太さ, 色, 線種] か 0）→ CSS。
fn border_css(borders: &Value) -> Vec<String> {
    let Some(sides) = borders.as_array().filter(|a| a.len() == 4) else {
        return Vec::new();
    };
    let line = |b: &Value| -> Option<String> {
        let w = b[0].as_f64()?;
        let kind = match b[2].as_i64().unwrap_or(0) {
            1 => "dotted",
            2 => "dashed",
            3 => "double",
            _ => "solid",
        };
        let color = hex(b[1].as_i64().unwrap_or(0)).unwrap_or_else(|| "#000000".into());
        Some(format!("{} {kind} {color}", pt_of(w)))
    };
    let lines: Vec<Option<String>> = sides.iter().map(line).collect();
    if lines.iter().all(|l| l.is_some() && *l == lines[0]) {
        return vec![format!("border: {}", lines[0].clone().unwrap_or_default())];
    }
    ["top", "bottom", "left", "right"]
        .iter()
        .zip(&lines)
        .filter_map(|(side, l)| l.as_ref().map(|l| format!("border-{side}: {l}")))
        .collect()
}

/// 表の列幅（区切りの位置と表の幅から mm で）。
fn column_widths_mm(t: &Value) -> Vec<f64> {
    let (Some(width), Some(sum)) = (t["width"].as_f64(), t["rel_sum"].as_f64()) else {
        return Vec::new();
    };
    if sum <= 0.0 {
        return Vec::new();
    }
    let mut stops: Vec<f64> = t["separators"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_f64).collect())
        .unwrap_or_default();
    stops.push(sum);
    let mut prev = 0.0;
    stops
        .into_iter()
        .map(|p| {
            let w = (p - prev) / sum * width / 100.0;
            prev = p;
            (w * 10.0).round() / 10.0
        })
        .collect()
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
    let mut out = String::from("<table");
    if let Some(w) = t["width"].as_f64() {
        let _ = write!(out, " style=\"width: {:.1}mm\"", w / 100.0);
    }
    if t["hori"].as_i64() == Some(2) {
        out.push_str(" align=\"center\"");
    }
    out.push('>');
    let cols = column_widths_mm(t);
    if cols.len() > 1 {
        out.push_str("<colgroup>");
        for w in &cols {
            let _ = write!(out, "<col width=\"{w}mm\">");
        }
        out.push_str("</colgroup>");
    }
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
        let mut css = Vec::new();
        if let Some(bg) = hex(c["bg"].as_i64().unwrap_or(-1)) {
            css.push(format!("background-color: {bg}"));
        }
        css.extend(border_css(&c["borders"]));
        match c["valign"].as_i64() {
            Some(2) => css.push("vertical-align: middle".into()),
            Some(3) => css.push("vertical-align: bottom".into()),
            _ => {}
        }
        if !css.is_empty() {
            let _ = write!(attrs, " style=\"{}\"", css.join("; "));
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
    if let Some(next) = v["truncated_at"].as_str() {
        let _ = writeln!(
            out,
            "※ 範囲が長いので {next} の手前までで打ち切りました。replace の範囲は上の最後のブロックまでにし、続きは {next} から読み直してください。"
        );
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use serde_json::json;

    #[test]
    fn marks_page_breaks_and_warns_about_a_small_overflow() {
        let block = |i: u64, page: u64, text: &str| json!({"t": "p", "id": format!("b{i}-0000"), "page": page, "text": text, "style": "Standard"});
        let ov = json!({
            "count": 3, "from": 0, "pages": 2, "page": {"Width": 21000, "Height": 29700},
            "blocks": [block(0, 1, "本文"), block(1, 1, "記"), block(2, 2, "電話：03-xxxx")],
        });
        let text = overview_text("", &ov);
        assert!(
            text.contains("── 1 ページ目 ──") && text.contains("── 2 ページ目 ──"),
            "{text}"
        );
        let w = page_warnings(&ov);
        assert_eq!(w.len(), 1);
        assert!(
            w[0].contains("b2-0000") && w[0].contains("1 ページに収めたい"),
            "{}",
            w[0]
        );
        // 途中から読んだ一覧・1 ページの文書では判定しない。
        let mut partial = ov.clone();
        partial["from"] = json!(1);
        assert!(page_warnings(&partial).is_empty());
        let mut single = ov.clone();
        single["pages"] = json!(1);
        assert!(page_warnings(&single).is_empty());
        assert!(!overview_text("", &single).contains("ページ目"));
    }

    #[test]
    fn list_start_uses_the_number_of_the_level() {
        assert_eq!(last_number("1.2."), "2");
        assert_eq!(last_number("(3)"), "3");
        assert_eq!(last_number("第12条"), "12");
        assert_eq!(last_number("・"), "");
    }

    #[test]
    fn content_continuing_from_the_previous_page_is_not_reported() {
        let ov = json!({"count": 2, "from": 0, "pages": 2, "page": {}, "blocks": [
            {"t": "table", "id": "b0-0000", "page": 1, "rows": 60, "cols": 3},
            {"t": "p", "id": "b1-0000", "page": 2, "text": "以上", "style": "Standard"},
        ]});
        assert!(page_warnings(&ov).is_empty(), "{:?}", page_warnings(&ov));
    }

    #[test]
    fn warns_about_a_page_that_holds_only_a_heading() {
        let block = |i: u64, page: u64, text: &str| json!({"t": "p", "id": format!("b{i}-0000"), "page": page, "text": text, "style": "Standard"});
        let mut blocks = vec![
            block(0, 1, "報告書"),
            block(1, 1, "2026年10月20日"),
            block(2, 2, "目次"),
        ];
        for i in 3..30 {
            blocks.push(block(
                i,
                3,
                "本文の段落がここに長く続いていて、最後のページはしっかり埋まっています。",
            ));
        }
        let ov =
            json!({"count": blocks.len(), "from": 0, "pages": 3, "page": {}, "blocks": blocks});
        let w = page_warnings(&ov);
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("2 ページ目には 「目次」 だけ"), "{}", w[0]);
    }

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
