//! 操作の受け付け（1 件ずつ解釈して誤りの位置を示す）と、高水準の操作の展開（#725）。

use serde_json::{json, Value};

use super::normalize::{Cx, OpError};
use super::ops::{HeaderFooter, SheetOp, TableTheme};

/// シートそのものを対象にする操作（`sheet` が「対象のシート名」であり、既定シートではない）。
const SHEET_TARGET_OPS: &[&str] = &[
    "rename_sheet",
    "delete_sheet",
    "move_sheet",
    "sheet_props",
    "insert_rows",
    "insert_columns",
    "autofilter",
    "freeze",
    "add_chart",
    "delete_chart",
    "page_setup",
];

/// 1 件の操作と、その操作に書かれた既定シート。
pub type ParsedOp = (SheetOp, Option<String>);

/// JSON の操作の並びを 1 件ずつ解釈する。誤りは何件目の何が悪いかを返す。
///
/// どの操作にも `sheet` を書いてよい（モデルが自然に書く形）。範囲にシート名が無いときの、
/// その操作だけの既定シートとして扱う（シート自体を対象にする操作では従来どおり対象名）。
pub fn parse_ops(values: &[Value]) -> Result<Vec<ParsedOp>, OpError> {
    let mut out = Vec::with_capacity(values.len());
    for (i, v) in values.iter().enumerate() {
        let op_name = v
            .get("op")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string();
        let mut v = v.clone();
        crate::engine::coerce_bools(&mut v);
        crate::engine::relax_op(&mut v);
        if op_name == "format" {
            flatten_format(&mut v);
        }
        if op_name == "?" {
            let guess = guess_op(&v);
            return Err(OpError {
                index: i + 1,
                op: "不明な操作",
                message: match guess {
                    Some(g) => format!("\"op\" が書かれていません（項目から見ると {g} のようです。{{\"op\": \"{g}\", ...}} と書いてください）"),
                    None => "\"op\" が書かれていません（各操作に {\"op\": \"set_values\", ...} のように操作名を書いてください）".into(),
                },
            });
        }
        let mut op_sheet = None;
        if !SHEET_TARGET_OPS.contains(&op_name.as_str()) {
            if let Some(obj) = v.as_object_mut() {
                op_sheet = obj
                    .remove("sheet")
                    .and_then(|s| s.as_str().map(str::to_string));
            }
        }
        match serde_json::from_value::<SheetOp>(v) {
            Ok(op) => out.push((op, op_sheet)),
            Err(e) => {
                return Err(OpError {
                    index: i + 1,
                    op: leak_name(&op_name),
                    message: format!("{e}（項目名と値の形を説明文のとおりにしてください。values は常に [[...]] の 2 次元です）"),
                })
            }
        }
    }
    Ok(out)
}

/// format の書き方の揺れを直す（弱いモデルがよく書く形・Gemma 4 の評価）:
/// - `{"op":"format","format":{"number_format":...}}` の入れ子は外へ出す
/// - `bold` / `italic` / `size` / `font_color` などを font の外に書いたら font へ入れる
fn flatten_format(v: &mut Value) {
    let Some(obj) = v.as_object_mut() else {
        return;
    };
    if let Some(Value::Object(inner)) = obj.remove("format") {
        for (k, x) in inner {
            obj.entry(k).or_insert(x);
        }
    }
    // 整列の項目（horizontal など）を align の外に書いたら align へ入れる。
    for key in [
        "horizontal",
        "vertical",
        "wrap",
        "shrink",
        "indent",
        "rotate",
    ] {
        if let Some(x) = obj.remove(key) {
            let align = obj
                .entry("align")
                .or_insert_with(|| Value::Object(serde_json::Map::new()));
            if let Some(a) = align.as_object_mut() {
                a.entry(key.to_string()).or_insert(x);
            }
        }
    }
    const FONT_KEYS: &[(&str, &str)] = &[
        ("bold", "bold"),
        ("italic", "italic"),
        ("underline", "underline"),
        ("strike", "strike"),
        ("size", "size"),
        ("font_size", "size"),
        ("font_color", "color"),
        ("font_name", "name"),
    ];
    for (from, to) in FONT_KEYS {
        if let Some(x) = obj.remove(*from) {
            let font = obj
                .entry("font")
                .or_insert_with(|| Value::Object(serde_json::Map::new()));
            if let Some(f) = font.as_object_mut() {
                f.entry((*to).to_string()).or_insert(x);
            }
        }
    }
}

/// op の無い操作の項目から、それらしい操作名を推す（案内に使うだけで、勝手に実行はしない）。
fn guess_op(v: &Value) -> Option<&'static str> {
    let has = |k: &str| v.get(k).is_some();
    if has("data") && has("anchor") {
        Some("add_chart")
    } else if has("formula") && has("range") {
        Some("fill_formula")
    } else if has("values") {
        Some("set_values")
    } else if has("number_format") || has("fill") || has("font") || has("border") {
        Some("format")
    } else if has("width") && has("columns") {
        Some("column_width")
    } else {
        None
    }
}

/// 誤りの表示に使う op 名（語彙の名前なら静的な文字列を返す）。
fn leak_name(name: &str) -> &'static str {
    const KNOWN: &[&str] = &[
        "set_values",
        "fill_formula",
        "format",
        "style_table",
        "merge",
        "unmerge",
        "column_width",
        "row_height",
        "insert_rows",
        "delete_rows",
        "insert_columns",
        "delete_columns",
        "hide_rows",
        "show_rows",
        "hide_columns",
        "show_columns",
        "clear",
        "sort",
        "autofilter",
        "freeze",
        "add_sheet",
        "rename_sheet",
        "delete_sheet",
        "move_sheet",
        "sheet_props",
        "conditional_format",
        "clear_conditional_formats",
        "data_validation",
        "define_name",
        "add_chart",
        "delete_chart",
        "page_setup",
    ];
    KNOWN
        .iter()
        .find(|k| **k == name)
        .copied()
        .unwrap_or("不明な操作")
}

/// 配色ごとの色（見出し・縞・合計行・罫線）。
pub(crate) fn palette(
    theme: TableTheme,
) -> (&'static str, &'static str, &'static str, &'static str) {
    match theme {
        TableTheme::Blue => ("#1F4E79", "#EAF1FB", "#DDEBF7", "#9DB7D5"),
        TableTheme::Navy => ("#1F3864", "#EDF0F7", "#D9DFEE", "#8EA0C8"),
        TableTheme::Green => ("#375623", "#EEF5E9", "#E2EFDA", "#A9C990"),
        TableTheme::Gray => ("#404040", "#F2F2F2", "#E7E6E6", "#BFBFBF"),
        TableTheme::Orange => ("#C55A11", "#FDF0E7", "#FBE5D6", "#F4B183"),
    }
}

impl Cx {
    /// `style_table` を書式の操作へ展開する（見出し・縞・合計行・罫線）。
    pub(super) fn style_table(
        &mut self,
        range: &str,
        theme: TableTheme,
        header_rows: u32,
        total_rows: u32,
        banded: bool,
    ) -> Result<Vec<Value>, OpError> {
        let a = self.area(range)?;
        // 縞は 1 行おきの矩形に展開するので、列全体（A:F）などの巨大な範囲を受けない。
        if a.cells() > super::normalize::MAX_CELLS_PER_APPLY {
            return Err(self.err(format!(
                "表の範囲が大きすぎます（{} セルまで。列全体ではなく表の範囲を指定してください）",
                super::normalize::MAX_CELLS_PER_APPLY
            )));
        }
        if a.height() < header_rows + total_rows + 1 || header_rows > 5 || total_rows > 5 {
            return Err(self.err("表の範囲には見出し（header_rows）・集計行（total_rows）と 1 行以上の本体が要ります"));
        }
        let (head, band, total, line) = palette(theme);
        let sheet = self.sheet(a.sheet.as_deref());
        let rect = |sr: u32, er: u32| json!([a.sc, sr, a.ec, er]);
        let thin =
            json!({"style": 0, "width": 26, "outer": 26, "inner": 0, "dist": 0, "color": line});
        let medium_head =
            json!({"style": 0, "width": 62, "outer": 62, "inner": 0, "dist": 0, "color": head});
        let double =
            json!({"style": 15, "width": 62, "outer": 18, "inner": 18, "dist": 26, "color": head});
        let mut ops = Vec::new();
        // 全体の罫線（外枠と内側を細線で）。
        ops.push(json!({"op": "format", "sheet": sheet, "areas": [a.coords()], "borders": {"outline": thin, "inner": thin}}));
        // 見出し。
        let head_end = a.sr + header_rows - 1;
        ops.push(json!({
            "op": "format", "sheet": sheet, "areas": [rect(a.sr, head_end)],
            "font": {"bold": true, "color": "#FFFFFF"}, "fill": head,
            "align": {"horizontal": "center", "vertical": "center", "wrap": true},
            "borders": {"bottom": medium_head},
        }));
        let body_end = a.er - total_rows;
        if banded {
            let rows: Vec<Value> = ((head_end + 1)..=body_end)
                .filter(|r| (r - head_end).is_multiple_of(2))
                .map(|r| rect(r, r))
                .collect();
            if !rows.is_empty() {
                ops.push(json!({"op": "format", "sheet": sheet, "areas": rows, "fill": band}));
            }
        }
        if total_rows > 0 {
            ops.push(json!({
                "op": "format", "sheet": sheet, "areas": [rect(body_end + 1, a.er)],
                "font": {"bold": true}, "fill": total,
            }));
            ops.push(json!({
                "op": "format", "sheet": sheet, "areas": [rect(body_end + 1, body_end + 1)],
                "borders": {"top": double},
            }));
        }
        Ok(ops)
    }
}

/// ヘッダー・フッターの文字列を、文字と差し込み（ページ番号など）の並びにする。
pub(super) fn header_footer_json(h: &HeaderFooter) -> Value {
    let parts = |s: &Option<String>| -> Value {
        let Some(s) = s else { return json!([]) };
        let mut out = Vec::new();
        let mut rest = s.as_str();
        while !rest.is_empty() {
            let next = ["&[Page]", "&[Pages]", "&[Date]", "&[Sheet]", "&[File]"]
                .iter()
                .filter_map(|t| rest.find(t).map(|i| (i, *t)))
                .min_by_key(|(i, _)| *i);
            if let Some((i, tag)) = next {
                {
                    if i > 0 {
                        out.push(json!({"t": "text", "v": &rest[..i]}));
                    }
                    let kind = match tag {
                        "&[Page]" => "page",
                        "&[Pages]" => "pages",
                        "&[Date]" => "date",
                        "&[Sheet]" => "sheet",
                        _ => "file",
                    };
                    out.push(json!({"t": kind}));
                    rest = &rest[i + tag.len()..];
                }
            } else {
                out.push(json!({"t": "text", "v": rest}));
                rest = "";
            }
        }
        Value::Array(out)
    };
    json!({"left": parts(&h.left), "center": parts(&h.center), "right": parts(&h.right)})
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use serde_json::json;

    #[test]
    fn nested_format_and_loose_font_keys_are_accepted() {
        let parsed = parse_ops(&[
            json!({"op": "format", "range": "C2:C7", "format": {"number_format": "yyyy/mm/dd"}}),
            json!({"op": "format", "range": "A8:F8", "bold": true, "font_color": "#FF0000"}),
        ])
        .unwrap();
        let items: Vec<_> = parsed.iter().map(|(o, s)| (o, s.as_deref())).collect();
        let n = super::super::normalize::normalize_with_sheets(&items, None).unwrap();
        assert_eq!(n.ops[0]["number_format"], json!("yyyy/mm/dd"));
        assert_eq!(n.ops[1]["font"]["bold"], json!(true));
        assert_eq!(n.ops[1]["font"]["color"], json!("#FF0000"));
        let parsed = parse_ops(&[
            json!({"op": "format", "range": "A1", "horizontal": "center", "summary": "x"}),
        ])
        .unwrap();
        let items: Vec<_> = parsed.iter().map(|(o, s)| (o, s.as_deref())).collect();
        let n = super::super::normalize::normalize_with_sheets(&items, None).unwrap();
        assert!(n.ops[0].to_string().contains("center"), "{}", n.ops[0]);
    }

    #[test]
    fn a_missing_op_names_the_likely_operation() {
        let e = parse_ops(&[json!({"data": "J1:N7", "anchor": "J2", "type": "line"})])
            .unwrap_err()
            .to_string();
        assert!(e.contains("add_chart"), "{e}");
    }
}
