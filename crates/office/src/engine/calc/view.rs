//! 読み取り結果（calc_read.js の生データ）を LLM 向けのテキストへ整形する（#725）。
//!
//! 方針: 値は表示文字列のグリッド（行番号・列名つき）、数式は Excel の書き方で別に並べ、
//! 同じ形の数式が縦・横に続くものはまとめる。書式は「最も広い書式」を基準に、それと違う範囲
//! だけを列挙する（空セルや既定の書式で行数を食わない）。

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde_json::Value;

use super::normalize::{mm100_to_chars, mm100_to_pt};
use crate::engine::a1::{cell_name, col_name, quote_sheet};
use crate::engine::formula::{calc_to_excel, relative_signature};

/// LO のエラーコード → Excel の表記と短い説明。
pub fn error_label(code: i64) -> (&'static str, &'static str) {
    match code {
        532 => ("#DIV/0!", "0 で割っています"),
        524 => ("#REF!", "参照先がありません"),
        525 => ("#NAME?", "関数名・名前が見つかりません"),
        519 | 502 => ("#VALUE!", "値の型が合いません"),
        503 => ("#NUM!", "数値として計算できません"),
        522 => ("循環参照", "自分自身を参照しています"),
        504 => ("#VALUE!", "関数の引数の数・形が合いません"),
        508 | 509 | 510 | 501 => ("#NAME?", "数式の書き方が誤っています（括弧・演算子）"),
        32767 => ("#N/A", "値が見つかりません"),
        _ => ("#ERR", "計算できません"),
    }
}

fn s(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}

fn u(v: &Value) -> u32 {
    v.as_u64().and_then(|n| u32::try_from(n).ok()).unwrap_or(0)
}

fn area_a1(a: &Value) -> String {
    let c: Vec<u32> = a
        .as_array()
        .map(|v| v.iter().map(u).collect())
        .unwrap_or_default();
    if c.len() != 4 {
        return "?".into();
    }
    if c[0] == c[2] && c[1] == c[3] {
        cell_name(c[0], c[1])
    } else {
        format!("{}:{}", cell_name(c[0], c[1]), cell_name(c[2], c[3]))
    }
}

fn hex(n: i64) -> Option<String> {
    (n >= 0).then(|| format!("#{:06X}", n & 0x00FF_FFFF))
}

/// ブックの概要（calc.overview の結果）。
pub fn overview_text(file_name: &str, v: &Value) -> String {
    let sheets = v["sheets"].as_array().cloned().unwrap_or_default();
    let mut out = String::new();
    let active = s(&v["active"]);
    let _ = writeln!(
        out,
        "ブック「{file_name}」: シート {} 枚（作業中のシート: {active}）",
        sheets.len()
    );
    for sh in &sheets {
        let name = s(&sh["name"]);
        let mut parts = Vec::new();
        match sh["used"].as_array() {
            Some(_) => parts.push(format!("使用範囲 {}", area_a1(&sh["used"]))),
            None => parts.push("空".into()),
        }
        if sh["visible"] == Value::Bool(false) {
            parts.push("非表示".into());
        }
        if let Some(f) = sh["freeze"].as_array() {
            parts.push(format!("固定 {}", cell_name(u(&f[0]), u(&f[1]))));
        }
        if let Some(c) = hex(sh["tab_color"].as_i64().unwrap_or(-1)) {
            parts.push(format!("見出しの色 {c}"));
        }
        let cf = sh["conditional_format_count"].as_u64().unwrap_or(0);
        if cf > 0 {
            parts.push(format!("条件付き書式 {cf}"));
        }
        let charts = sh["charts"].as_array().cloned().unwrap_or_default();
        for ch in &charts {
            parts.push(chart_line(ch));
        }
        let _ = writeln!(out, "- {}: {}", quote_sheet(name), parts.join("・"));
    }
    let names = v["names"].as_array().cloned().unwrap_or_default();
    if !names.is_empty() {
        let list: Vec<String> = names
            .iter()
            .map(|n| {
                format!(
                    "{} = {}",
                    s(&n["name"]),
                    calc_to_excel(&format!("={}", s(&n["content"]))).trim_start_matches('=')
                )
            })
            .collect();
        let _ = writeln!(out, "名前の定義: {}", list.join("、"));
    }
    out
}

fn chart_line(ch: &Value) -> String {
    let kind = match (s(&ch["diagram"]), ch["Vertical"].as_bool().unwrap_or(false)) {
        ("BarDiagram", true) => "横棒",
        ("BarDiagram", false) => "縦棒",
        ("LineDiagram", _) => "折れ線",
        ("AreaDiagram", _) => "面",
        ("PieDiagram", _) => "円",
        ("DonutDiagram", _) => "ドーナツ",
        ("XYDiagram", _) => "散布図",
        _ => "グラフ",
    };
    let ranges: Vec<String> = ch["ranges"]
        .as_array()
        .map(|rs| {
            rs.iter()
                .map(|r| {
                    let c: Vec<u32> = r
                        .as_array()
                        .map(|v| v[1..].iter().map(u).collect())
                        .unwrap_or_default();
                    let a = if c.len() == 4 {
                        format!("{}:{}", cell_name(c[0], c[1]), cell_name(c[2], c[3]))
                    } else {
                        "?".into()
                    };
                    format!("{}!{a}", quote_sheet(s(&r[0])))
                })
                .collect()
        })
        .unwrap_or_default();
    let pos = match (ch["anchor"].as_array(), ch["anchor_end"].as_array()) {
        (Some(a), Some(b)) => format!(
            " 位置 {}〜{}",
            cell_name(u(&a[0]), u(&a[1])),
            cell_name(u(&b[0]), u(&b[1]))
        ),
        _ => String::new(),
    };
    let title = ch["title"]
        .as_str()
        .map(|t| format!(" 題「{t}」"))
        .unwrap_or_default();
    format!(
        "グラフ「{}」{kind} データ {}{pos}{title}",
        s(&ch["name"]),
        ranges.join(",")
    )
}

/// 範囲の読み取り（calc.read_range の結果）。
pub fn range_text(v: &Value) -> String {
    let mut out = String::new();
    let sheet = s(&v["sheet"]);
    let Some(area) = v["area"].as_array() else {
        let _ = writeln!(
            out,
            "{}: 指定の範囲に値・書式はありません",
            quote_sheet(sheet)
        );
        return out;
    };
    let [sc, sr, ec, er] = [u(&area[0]), u(&area[1]), u(&area[2]), u(&area[3])];
    let used = v["used"]
        .as_array()
        .map(|_| area_a1(&v["used"]))
        .unwrap_or_default();
    let _ = writeln!(
        out,
        "{}!{}（シートの使用範囲 {used}）",
        quote_sheet(sheet),
        area_a1(&v["area"])
    );

    // セル: (行, 列) → (種類, 生の値/数式, 表示, エラー)
    let mut grid: Grid<'_> = BTreeMap::new();
    for c in v["cells"].as_array().into_iter().flatten() {
        let Some(c) = c.as_array() else { continue };
        let err = c.get(5).and_then(Value::as_i64);
        grid.insert((u(&c[1]), u(&c[0])), (s(&c[2]), &c[3], s(&c[4]), err));
    }
    if grid.is_empty() {
        let _ = writeln!(out, "（値の入ったセルはありません）");
    } else {
        write_grid(&mut out, &grid, sc, ec, sr, er);
    }
    if v["truncated"] == Value::Bool(true) {
        let _ = writeln!(
            out,
            "※ セル数の上限で途中までです。範囲を絞って続きを読んでください。"
        );
    }
    write_formulas(&mut out, &grid);
    write_errors(&mut out, &grid);
    if let Some(groups) = v["formats"].as_array() {
        write_formats(&mut out, groups, &v["default_format"]);
    }
    if let Some(m) = v["merges"].as_array().filter(|m| !m.is_empty()) {
        let list: Vec<String> = m.iter().map(area_a1).collect();
        let _ = writeln!(out, "結合: {}", list.join("、"));
    }
    write_dimensions(&mut out, v);
    write_sheet_features(&mut out, v);
    out
}

type Grid<'a> = BTreeMap<(u32, u32), (&'a str, &'a Value, &'a str, Option<i64>)>;

fn write_grid(out: &mut String, grid: &Grid<'_>, sc: u32, ec: u32, sr: u32, er: u32) {
    // 値の入った列・行だけを並べる（空の列で横幅を食わない）。
    let cols: Vec<u32> = (sc..=ec)
        .filter(|c| grid.keys().any(|(_, cc)| cc == c))
        .collect();
    let _ = writeln!(
        out,
        "     | {}",
        cols.iter()
            .map(|c| col_name(*c))
            .collect::<Vec<_>>()
            .join(" | ")
    );
    for r in sr..=er {
        if !grid.keys().any(|(rr, _)| *rr == r) {
            continue;
        }
        let cells: Vec<String> = cols
            .iter()
            .map(|c| {
                grid.get(&(r, *c))
                    .map(|(_, _, display, _)| display.replace('\n', "⏎").replace('|', "｜"))
                    .unwrap_or_default()
            })
            .collect();
        let _ = writeln!(out, "{:>4} | {}", r + 1, cells.join(" | "));
    }
}

/// 数式を Excel の書き方で並べる。同じ列で同じ形（相対参照で一致）の数式が続くものはまとめる。
fn write_formulas(out: &mut String, grid: &Grid<'_>) {
    let mut by_col: BTreeMap<u32, Vec<(u32, String, String)>> = BTreeMap::new();
    for ((r, c), (kind, raw, _, _)) in grid {
        if *kind != "f" {
            continue;
        }
        let excel = calc_to_excel(s(raw));
        // 相対参照をセルからの差に置き換えた形で比べる。
        let canon = relative_signature(&excel, *c, *r);
        by_col.entry(*c).or_default().push((*r, excel, canon));
    }
    if by_col.is_empty() {
        return;
    }
    let _ = writeln!(out, "数式:");
    let mut lines = 0;
    for (c, list) in by_col {
        let mut i = 0;
        while i < list.len() {
            let mut j = i;
            while j + 1 < list.len() && list[j + 1].0 == list[j].0 + 1 && list[j + 1].2 == list[i].2
            {
                j += 1;
            }
            let (r0, f0, _) = &list[i];
            if lines >= 60 {
                let _ = writeln!(out, "  …（以下略）");
                return;
            }
            if j > i {
                let _ = writeln!(
                    out,
                    "  {}:{} {}（{} 行、下へ同じ形）",
                    cell_name(c, *r0),
                    cell_name(c, list[j].0),
                    f0,
                    j - i + 1
                );
            } else {
                let _ = writeln!(out, "  {} {}", cell_name(c, *r0), f0);
            }
            lines += 1;
            i = j + 1;
        }
    }
}

fn write_errors(out: &mut String, grid: &Grid<'_>) {
    let errs: Vec<String> = grid
        .iter()
        .filter_map(|((r, c), (_, _, _, e))| {
            e.map(|code| {
                let (label, why) = error_label(code);
                format!("{} {label}（{why}）", cell_name(*c, *r))
            })
        })
        .collect();
    if !errs.is_empty() {
        let _ = writeln!(out, "エラー: {}", errs.join("、"));
    }
}

/// 書式の 1 グループを「既定との差分」の文言にする。
fn describe_format(p: &Value, base: &Value) -> Vec<String> {
    let mut d = Vec::new();
    let differs = |k: &str| p[k] != base[k];
    if differs("NumberFormat") {
        let nf = s(&p["NumberFormat"]);
        d.push(if nf == "Standard" || nf == "General" {
            "表示形式 標準".into()
        } else {
            format!("表示形式 {}", display_number_format(nf))
        });
    }
    // 和文（Asian）と欧文で別のフォントを持つ。日本語の文書では和文の違いを先に見せる。
    if differs("CharFontNameAsian") {
        d.push(format!("フォント {}", s(&p["CharFontNameAsian"])));
    }
    if differs("CharFontName") {
        d.push(format!("欧文フォント {}", s(&p["CharFontName"])));
    }
    if differs("CharHeight") {
        d.push(format!("{}pt", p["CharHeight"]));
    }
    if differs("CharWeight") {
        d.push(if p["CharWeight"].as_f64().unwrap_or(100.0) > 100.0 {
            "太字".into()
        } else {
            "標準の太さ".into()
        });
    }
    if differs("CharPosture") && s(&p["CharPosture"]) != "NONE" {
        d.push("斜体".into());
    }
    if differs("CharUnderline") && p["CharUnderline"].as_i64().unwrap_or(0) != 0 {
        d.push("下線".into());
    }
    if differs("CharColor") {
        d.push(
            hex(p["CharColor"].as_i64().unwrap_or(-1))
                .map_or("文字色 自動".into(), |c| format!("文字色 {c}")),
        );
    }
    let fill = |v: &Value| {
        if v["IsCellBackgroundTransparent"] == Value::Bool(true) {
            -1
        } else {
            v["CellBackColor"].as_i64().unwrap_or(-1)
        }
    };
    if fill(p) != fill(base) {
        d.push(hex(fill(p)).map_or("塗りなし".into(), |c| format!("塗り {c}")));
    }
    if differs("HoriJustify") {
        let h = match s(&p["HoriJustify"]) {
            "LEFT" => "左揃え",
            "CENTER" => "中央揃え",
            "RIGHT" => "右揃え",
            "BLOCK" => "両端揃え",
            _ => "横位置 標準",
        };
        d.push(h.into());
    }
    if differs("VertJustify") {
        let v = match p["VertJustify"].as_i64().unwrap_or(0) {
            1 => "上詰め",
            2 => "上下中央",
            3 => "下詰め",
            _ => "縦位置 標準",
        };
        d.push(v.into());
    }
    if differs("IsTextWrapped") && p["IsTextWrapped"] == Value::Bool(true) {
        d.push("折り返し".into());
    }
    if differs("ShrinkToFit") && p["ShrinkToFit"] == Value::Bool(true) {
        d.push("縮小して全体を表示".into());
    }
    let border = |side: &str| -> Option<String> {
        let b = &p[side];
        let w = b["w"].as_i64().unwrap_or(0);
        (w > 0).then(|| {
            // LineStyle: 1=点線, 2/14=破線, 3/15 など=二重線（com.sun.star.table.BorderLineStyle）。
            let kind = match (b["style"].as_i64().unwrap_or(0), w) {
                (3 | 7..=13 | 15 | 16, _) => "二重線",
                (2 | 14 | 17 | 18, _) => "破線",
                (1, _) => "点線",
                (_, 0..=8) => "極細線",
                (_, 9..=45) => "細線",
                (_, 46..=75) => "中線",
                _ => "太線",
            };
            format!(
                "{kind} {}",
                hex(b["color"].as_i64().unwrap_or(0)).unwrap_or_default()
            )
        })
    };
    let sides = ["TopBorder2", "BottomBorder2", "LeftBorder2", "RightBorder2"].map(border);
    if sides.iter().any(Option::is_some)
        && ["TopBorder2", "BottomBorder2", "LeftBorder2", "RightBorder2"]
            .iter()
            .any(|k| differs(k))
    {
        if sides.iter().all(|x| x == &sides[0]) {
            d.push(format!(
                "罫線 {}（各セルの上下左右）",
                sides[0].clone().unwrap_or_default()
            ));
        } else {
            let names = ["上", "下", "左", "右"];
            let parts: Vec<String> = sides
                .iter()
                .zip(names)
                .filter_map(|(x, n)| x.as_ref().map(|x| format!("{n} {x}")))
                .collect();
            d.push(format!("罫線 {}", parts.join("・")));
        }
    }
    if let Some(v) = p.get("Validation").filter(|v| v.is_object()) {
        d.push(validation_text(v));
    }
    d
}

fn validation_text(v: &Value) -> String {
    let f1 = calc_to_excel(&format!("={}", s(&v["formula1"])));
    let f1 = f1.trim_start_matches('=');
    match s(&v["type"]) {
        "LIST" => format!("入力規則 リスト {f1}"),
        "WHOLE" => format!("入力規則 整数 {} {f1}", s(&v["operator"])),
        "DECIMAL" => format!("入力規則 小数 {} {f1}", s(&v["operator"])),
        "DATE" => format!("入力規則 日付 {} {f1}", s(&v["operator"])),
        "TEXT_LEN" => format!("入力規則 文字数 {} {f1}", s(&v["operator"])),
        other => format!("入力規則 {other} {f1}"),
    }
}

fn write_formats(out: &mut String, groups: &[Value], default: &Value) {
    // 基準 = 文書の既定の書式（使用範囲の外の空セル）。読めなければ最も広い書式。
    let size = |g: &Value| -> u64 {
        let area = |r: &Value| -> u64 {
            let c: Vec<u64> = r
                .as_array()
                .map(|v| v.iter().filter_map(Value::as_u64).collect())
                .unwrap_or_default();
            if c.len() == 4 {
                (c[2] - c[0] + 1) * (c[3] - c[1] + 1)
            } else {
                0
            }
        };
        g["ranges"]
            .as_array()
            .map_or(0, |rs| rs.iter().map(area).sum())
    };
    let Some(widest) = groups.iter().max_by_key(|g| size(g)) else {
        return;
    };
    let base_props = if default.is_object() {
        default
    } else {
        &widest["props"]
    };
    // 属性 → 範囲 に裏返す（同じ罫線が全グループに繰り返されるのを 1 行にまとめる）。
    let mut by_attr: Vec<(String, Vec<[u64; 4]>)> = Vec::new();
    for g in groups {
        let rects: Vec<[u64; 4]> = g["ranges"]
            .as_array()
            .map(|rs| {
                rs.iter()
                    .filter_map(|r| {
                        let c: Vec<u64> = r.as_array()?.iter().filter_map(Value::as_u64).collect();
                        (c.len() == 4).then(|| [c[0], c[1], c[2], c[3]])
                    })
                    .collect()
            })
            .unwrap_or_default();
        for attr in describe_format(&g["props"], base_props) {
            match by_attr.iter_mut().find(|(a, _)| *a == attr) {
                Some((_, list)) => list.extend(rects.iter().copied()),
                None => by_attr.push((attr, rects.clone())),
            }
        }
    }
    let base_font = s(&base_props["CharFontNameAsian"]);
    let _ = writeln!(
        out,
        "書式（基準: フォント {base_font} {}pt・表示形式 {}。以下は基準と違うもの）:",
        base_props["CharHeight"],
        s(&base_props["NumberFormat"])
    );
    if by_attr.is_empty() {
        let _ = writeln!(out, "  （基準と違う書式はありません）");
    }
    for (i, (attr, rects)) in by_attr.iter().enumerate() {
        if i >= 40 {
            let _ = writeln!(out, "  …（ほか {} 件）", by_attr.len() - i);
            break;
        }
        let _ = writeln!(out, "  {attr}: {}", rects_text(rects));
    }
}

/// 長方形の並びを A1 表記にする。隙間なく 1 つの長方形を埋めるならまとめる。
fn rects_text(rects: &[[u64; 4]]) -> String {
    let to_a1 = |r: &[u64; 4]| -> String {
        let n = |v: u64| u32::try_from(v).unwrap_or(0);
        if r[0] == r[2] && r[1] == r[3] {
            cell_name(n(r[0]), n(r[1]))
        } else {
            format!(
                "{}:{}",
                cell_name(n(r[0]), n(r[1])),
                cell_name(n(r[2]), n(r[3]))
            )
        }
    };
    merge_rects(rects)
        .iter()
        .map(to_a1)
        .collect::<Vec<_>>()
        .join(",")
}

/// 辺を共有する長方形を、それ以上まとまらなくなるまでまとめる（A5,A6,A7 → A5:A7）。
fn merge_rects(rects: &[[u64; 4]]) -> Vec<[u64; 4]> {
    let mut list: Vec<[u64; 4]> = rects.to_vec();
    loop {
        let mut merged = false;
        'outer: for i in 0..list.len() {
            for j in (i + 1)..list.len() {
                let (a, b) = (list[i], list[j]);
                let vertical =
                    a[0] == b[0] && a[2] == b[2] && (a[3] + 1 == b[1] || b[3] + 1 == a[1]);
                let horizontal =
                    a[1] == b[1] && a[3] == b[3] && (a[2] + 1 == b[0] || b[2] + 1 == a[0]);
                if vertical || horizontal {
                    list[i] = [
                        a[0].min(b[0]),
                        a[1].min(b[1]),
                        a[2].max(b[2]),
                        a[3].max(b[3]),
                    ];
                    list.remove(j);
                    merged = true;
                    break 'outer;
                }
            }
        }
        if !merged {
            break;
        }
    }
    list.sort_by_key(|r| (r[1], r[0]));
    list
}

/// 表示形式を、書いたときの形に近づけて見せる（kit は `年` を `\\年` のようにエスケープして返す）。
fn display_number_format(code: &str) -> String {
    let mut out = String::with_capacity(code.len());
    let mut chars = code.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' && chars.peek().is_some_and(|n| !n.is_ascii()) {
            continue;
        }
        out.push(c);
    }
    out
}

fn write_dimensions(out: &mut String, v: &Value) {
    let cols: Vec<(u32, f64, bool)> = v["columns"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| {
            c.as_array().map(|c| {
                (
                    u(&c[0]),
                    mm100_to_chars(u(&c[1])),
                    c[2] != Value::Bool(false),
                )
            })
        })
        .collect();
    if !cols.is_empty() {
        let mut parts: Vec<String> = Vec::new();
        let mut i = 0;
        while i < cols.len() {
            let mut j = i;
            while j + 1 < cols.len()
                && (cols[j + 1].1 - cols[i].1).abs() < 0.05
                && cols[j + 1].2 == cols[i].2
            {
                j += 1;
            }
            let name = if j > i {
                format!("{}:{}", col_name(cols[i].0), col_name(cols[j].0))
            } else {
                col_name(cols[i].0)
            };
            parts.push(if cols[i].2 {
                format!("{name}={}", cols[i].1)
            } else {
                format!("{name}=非表示")
            });
            i = j + 1;
        }
        let _ = writeln!(out, "列幅（文字数）: {}", parts.join(" "));
    }
    let rows: Vec<String> = v["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| {
            r.as_array().map(|r| {
                if r[2] == Value::Bool(false) {
                    format!("{}=非表示", u(&r[0]) + 1)
                } else {
                    format!("{}={}pt", u(&r[0]) + 1, mm100_to_pt(u(&r[1])))
                }
            })
        })
        .collect();
    if !rows.is_empty() {
        let _ = writeln!(out, "行の高さ（指定あり）: {}", rows.join(" "));
    }
}

fn write_sheet_features(out: &mut String, v: &Value) {
    if let Some(f) = v["freeze"].as_array() {
        let _ = writeln!(out, "固定: {}", cell_name(u(&f[0]), u(&f[1])));
    }
    for cf in v["conditional_formats"].as_array().into_iter().flatten() {
        let ranges: Vec<String> = cf["ranges"]
            .as_array()
            .map(|rs| rs.iter().map(area_a1).collect())
            .unwrap_or_default();
        for e in cf["entries"].as_array().into_iter().flatten() {
            let look = &e["style"];
            let mut lk = Vec::new();
            // 文字色 0（黒）は「既定のまま」と区別できないので出さない。
            if let Some(c) =
                hex(look["CharColor"].as_i64().unwrap_or(-1)).filter(|c| c != "#000000")
            {
                lk.push(format!("文字色 {c}"));
            }
            if let Some(c) = hex(look["CellBackColor"].as_i64().unwrap_or(-1)) {
                lk.push(format!("塗り {c}"));
            }
            if look["CharWeight"].as_f64().unwrap_or(100.0) > 100.0 {
                lk.push("太字".into());
            }
            let cond = match e["type"].as_i64() {
                Some(0) => format!(
                    "条件 {} {} {}",
                    operator_label(e["Operator"].as_i64().unwrap_or(-1)),
                    calc_to_excel(&format!("={}", s(&e["Formula1"]))).trim_start_matches('='),
                    e["Formula2"]
                        .as_str()
                        .filter(|f| !f.is_empty() && *f != "0")
                        .unwrap_or("")
                ),
                Some(1) => "カラースケール".into(),
                Some(2) => "データバー".into(),
                Some(3) => "アイコン".into(),
                _ => "その他の条件".into(),
            };
            let _ = writeln!(
                out,
                "条件付き書式: {} {} → {}",
                ranges.join(","),
                cond.trim_end(),
                lk.join("・")
            );
        }
    }
    for ch in v["charts"].as_array().into_iter().flatten() {
        let _ = writeln!(out, "{}", chart_line(ch));
    }
    let page = &v["page"];
    if page.is_object() {
        let mut print = Vec::new();
        let (w, h) = (
            page["Width"].as_u64().unwrap_or(0),
            page["Height"].as_u64().unwrap_or(0),
        );
        let paper = match (w.min(h), w.max(h)) {
            (20900..=21100, 29600..=29800) => "A4",
            (29600..=29800, 41900..=42100) => "A3",
            (18100..=18300, 25600..=25800) => "B5",
            (25600..=25800, 36300..=36500) => "B4",
            _ => "用紙",
        };
        print.push(format!(
            "{paper}{}",
            if page["IsLandscape"] == Value::Bool(true) {
                "横"
            } else {
                "縦"
            }
        ));
        let (fx, fy) = (
            page["ScaleToPagesX"].as_u64().unwrap_or(0),
            page["ScaleToPagesY"].as_u64().unwrap_or(0),
        );
        if fx > 0 || fy > 0 {
            let n = |v: u64| {
                if v == 0 {
                    "制限なし".to_string()
                } else {
                    v.to_string()
                }
            };
            print.push(format!("幅 {} × 高さ {} ページに収める", n(fx), n(fy)));
        } else if let Some(sc) = page["PageScale"].as_u64().filter(|s| *s != 100) {
            print.push(format!("{sc}%"));
        }
        if let Some(pa) = page["print_areas"].as_array().filter(|a| !a.is_empty()) {
            print.push(format!(
                "印刷範囲 {}",
                pa.iter().map(area_a1).collect::<Vec<_>>().join(",")
            ));
        }
        if let Some(tr) = page["title_rows"].as_array() {
            print.push(format!("見出し行 {}:{}", u(&tr[1]) + 1, u(&tr[3]) + 1));
        }
        let _ = writeln!(out, "印刷: {}", print.join("・"));
    }
}

fn operator_label(op: i64) -> &'static str {
    // 読み取りは新しい API（XConditionEntry）なので com.sun.star.sheet.ConditionFormatOperator の値。
    match op {
        0 => "=",
        1 => "<",
        2 => ">",
        3 => "<=",
        4 => ">=",
        5 => "<>",
        6 => "between",
        7 => "not_between",
        8 => "重複",
        9 => "一意",
        10 => "上位",
        11 => "下位",
        14 => "平均より上",
        15 => "平均より下",
        18 => "エラー",
        22 => "含む",
        24 => "数式",
        _ => "",
    }
}

/// 表示文字列のおおよその幅（px・Calibri 11pt 相当。全角は 2 倍）。
pub fn text_width_px(text: &str) -> f64 {
    text.chars()
        .map(|c| if c.is_ascii() { 7.0 } else { 14.0 })
        .sum::<f64>()
        + 5.0
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use serde_json::json;

    fn read_result() -> Value {
        json!({
            "sheet": "売上", "area": [0, 0, 4, 4], "used": [0, 0, 4, 4], "truncated": false,
            "cells": [
                [0,0,"s",null,"支店"],[1,0,"s",null,"4月"],[4,0,"s",null,"合計"],
                [0,1,"s",null,"東京"],[1,1,"n",1200,"1,200"],[4,1,"f","=SUM(B2:D2)","4,050"],
                [0,2,"s",null,"大阪"],[1,2,"n",800,"800"],[4,2,"f","=SUM(B3:D3)","2,450"],
                [0,3,"s",null,"名古屋"],[1,3,"n",500,"500"],[4,3,"f","=SUM(B4:D4)","1,570"],
                [1,4,"f","=1/0","#DIV/0!",532],[2,4,"f","=SUM($集計.A1:A3)","0"]
            ],
            "formats": [
                {"ranges": [[0,0,4,0]], "props": {"CharFontNameAsian":"Noto Sans JP","CharHeight":11,"CharWeight":150,"CharColor":16_777_215,"CellBackColor":2_046_052,"IsCellBackgroundTransparent":false,"HoriJustify":"CENTER","VertJustify":0,"NumberFormat":"Standard"}},
                {"ranges": [[0,1,4,4]], "props": {"CharFontNameAsian":"Noto Sans JP","CharHeight":11,"CharWeight":100,"CharColor":-1,"CellBackColor":-1,"IsCellBackgroundTransparent":true,"HoriJustify":"STANDARD","VertJustify":0,"NumberFormat":"Standard"}},
                {"ranges": [[1,1,4,3]], "props": {"CharFontNameAsian":"Noto Sans JP","CharHeight":11,"CharWeight":100,"CharColor":-1,"CellBackColor":-1,"IsCellBackgroundTransparent":true,"HoriJustify":"STANDARD","VertJustify":0,"NumberFormat":"#,##0"}}
            ],
            "merges": [[0,6,4,6]],
            "columns": [[0, 2500, true], [1, 1693, true], [2, 1693, true], [3, 1693, false], [4, 2200, true]],
            "rows": [[0, 801, true]]
        })
    }

    #[test]
    fn renders_grid_formulas_errors_and_formats() {
        let text = range_text(&read_result());
        assert!(text.contains("売上!A1:E5"), "{text}");
        assert!(text.contains("   2 | 東京 | 1,200 |  | 4,050"), "{text}");
        assert!(
            text.contains("E2:E4 =SUM(B2:D2)（3 行、下へ同じ形）"),
            "{text}"
        );
        assert!(text.contains("C5 =SUM(集計!A1:A3)"), "{text}");
        assert!(text.contains("B5 #DIV/0!（0 で割っています）"), "{text}");
        assert!(text.contains("太字: A1:E1"), "{text}");
        assert!(text.contains("塗り #1F3864: A1:E1"), "{text}");
        assert!(text.contains("表示形式 #,##0: B2:E4"), "{text}");
        assert!(text.contains("結合: A7:E7"), "{text}");
        assert!(text.contains("B:C=8.4"), "{text}");
        assert!(text.contains("D=非表示"), "{text}");
        assert!(text.contains("1=22.7pt"), "{text}");
    }

    #[test]
    fn renders_overview() {
        let v = json!({
            "active": "売上",
            "sheets": [
                {"name": "売上", "used": [0,0,5,119], "visible": true, "tab_color": -1, "freeze": [1,1], "conditional_format_count": 2,
                 "charts": [{"name":"推移","diagram":"LineDiagram","Vertical":false,"ranges":[["売上",0,0,1,12]],"anchor":[7,1],"anchor_end":[14,20],"title":"月次"}]},
                {"name": "集計 2", "used": null, "visible": true, "tab_color": 7_384_391, "freeze": null, "conditional_format_count": 0, "charts": []}
            ],
            "names": [{"name": "売上表", "content": "$売上.$A$1:$E$5"}]
        });
        let text = overview_text("売上.xlsx", &v);
        assert!(
            text.contains("シート 2 枚（作業中のシート: 売上）"),
            "{text}"
        );
        assert!(text.contains("- 売上: 使用範囲 A1:F120・固定 B2・条件付き書式 2・グラフ「推移」折れ線 データ 売上!A1:B13 位置 H2〜O21 題「月次」"), "{text}");
        assert!(
            text.contains("- '集計 2': 空・見出しの色 #70AD47"),
            "{text}"
        );
        assert!(
            text.contains("名前の定義: 売上表 = 売上!$A$1:$E$5"),
            "{text}"
        );
    }
}

#[cfg(test)]
mod merge_tests {
    use super::*;

    #[test]
    fn merges_adjacent_rectangles() {
        let r = merge_rects(&[
            [0, 4, 0, 4],
            [0, 6, 0, 6],
            [0, 5, 0, 5],
            [0, 7, 0, 7],
            [2, 1, 2, 1],
        ]);
        assert_eq!(r, vec![[2, 1, 2, 1], [0, 4, 0, 7]]);
        assert_eq!(
            rects_text(&[[0, 0, 0, 0], [1, 0, 1, 0], [0, 1, 1, 1]]),
            "A1:B2"
        );
    }

    #[test]
    fn unescapes_number_formats_for_display() {
        assert_eq!(display_number_format("YYYY\\年M\\月D\\日"), "YYYY年M月D日");
        assert_eq!(
            display_number_format("#,##0;[RED]\\▲#,##0"),
            "#,##0;[RED]▲#,##0"
        );
        assert_eq!(display_number_format("0\\.0"), "0\\.0");
    }
}
