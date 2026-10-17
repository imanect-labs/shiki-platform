//! Word 操作の検証と、エンジン（writer_apply.js）が受け取る形への正規化（#725）。

use serde_json::{json, Map, Value};

use super::html::normalize_html;
use super::ops::{Align, DocOp, Edge, HeaderFooter, ParaSpec, StyleFamily, TextFont};
use crate::engine::calc::normalize::{normalize_color, OpError};
use crate::engine::calc::ops::{Orientation, Paper};

/// 正規化の結果。
#[derive(Debug, Clone)]
pub struct DocNormalized {
    pub ops: Vec<Value>,
    /// 操作が参照するブロック ID（エンジンが一括の最初にまとめて解決する）。
    pub block_ids: Vec<String>,
    /// 各エンジン操作の元になった操作（1 始まりの番号と名前）。
    pub origin: Vec<(usize, &'static str)>,
}

/// JSON の操作の並びを 1 件ずつ解釈して正規化する。
pub fn normalize_doc_ops(values: &[Value]) -> Result<DocNormalized, OpError> {
    let mut out = DocNormalized {
        ops: Vec::new(),
        block_ids: Vec::new(),
        origin: Vec::new(),
    };
    for (i, v) in values.iter().enumerate() {
        let name = v
            .get("op")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string();
        let op: DocOp = serde_json::from_value(v.clone()).map_err(|e| OpError {
            index: i + 1,
            op: known_name(&name),
            message: format!("{e}（項目名と値の形を説明文のとおりにしてください）"),
        })?;
        let mut cx = Cx {
            index: i + 1,
            op: op.name(),
            ids: &mut out.block_ids,
        };
        let produced = cx.one(&op)?;
        out.origin
            .extend(std::iter::repeat_n((i + 1, op.name()), produced.len()));
        out.ops.extend(produced);
    }
    Ok(out)
}

fn known_name(name: &str) -> &'static str {
    const KNOWN: &[&str] = &[
        "insert",
        "replace",
        "delete",
        "replace_text",
        "format_text",
        "format_paragraph",
        "set_style",
        "page_setup",
        "header",
        "footer",
        "insert_toc",
        "update_toc",
        "page_break",
        "track_changes",
        "title_page",
        "insert_rows",
        "delete_rows",
        "set_cells",
        "format_cells",
        "table_props",
        "style_table",
        "accept_all_changes",
        "reject_all_changes",
    ];
    KNOWN
        .iter()
        .find(|k| **k == name)
        .copied()
        .unwrap_or("不明な操作")
}

struct Cx<'a> {
    index: usize,
    op: &'static str,
    ids: &'a mut Vec<String>,
}

/// ブロック ID の形（`b12-3fa9`）か。
fn is_block_id(s: &str) -> bool {
    let Some(rest) = s.strip_prefix('b') else {
        return false;
    };
    let Some((n, h)) = rest.split_once('-') else {
        return false;
    };
    !n.is_empty()
        && n.chars().all(|c| c.is_ascii_digit())
        && h.len() == 4
        && h.chars().all(|c| c.is_ascii_hexdigit())
}

/// `target: "all"` の両端（エンジンの resolveBlock が最初と最後のブロックに解決する）。
const FIRST_BLOCK: &str = "@first";
const LAST_BLOCK: &str = "@last";

impl Cx<'_> {
    fn err(&self, message: impl Into<String>) -> OpError {
        OpError {
            index: self.index,
            op: self.op,
            message: message.into(),
        }
    }

    fn id(&mut self, s: &str) -> Result<String, OpError> {
        let s = s.trim().to_ascii_lowercase();
        if !is_block_id(&s) {
            return Err(self.err(format!(
                "ブロック ID「{s}」の形が不正です。ID は office.read の一覧の [ ] の中身をそのまま書きます\
                 （例 b12-3fa9。番号とハッシュは同じブロックのもの）。範囲は「最初の ID:最後の ID」\
                 （例 b0-0388:b15-fa60）、文書全体は \"all\""
            )));
        }
        if !self.ids.contains(&s) {
            self.ids.push(s.clone());
        }
        Ok(s)
    }

    /// `b1-xxxx` か `b1-xxxx:b5-yyyy` → (from, to)。`all` は文書の最初から最後まで
    /// （エンジンが適用の時点の最初と最後のブロックに解決する）。
    fn target(&mut self, s: &str) -> Result<(String, Option<String>), OpError> {
        if s.trim().eq_ignore_ascii_case("all") {
            for id in [FIRST_BLOCK, LAST_BLOCK] {
                if !self.ids.iter().any(|x| x == id) {
                    self.ids.push(id.to_string());
                }
            }
            return Ok((FIRST_BLOCK.to_string(), Some(LAST_BLOCK.to_string())));
        }
        match s.split_once(':') {
            Some((a, b)) => Ok((self.id(a)?, Some(self.id(b)?))),
            None => Ok((self.id(s)?, None)),
        }
    }

    fn color(&self, c: &str) -> Result<String, OpError> {
        normalize_color(c).ok_or_else(|| self.err(format!("色「{c}」は #RRGGBB で書いてください")))
    }

    fn html(&self, html: &str) -> Result<String, OpError> {
        normalize_html(html).map_err(|e| self.err(e.to_string()))
    }

    /// 位置（after / before / at のどれか 1 つ）→ (anchor, before, at)。
    fn position(
        &mut self,
        after: Option<&String>,
        before: Option<&String>,
        at: Option<Edge>,
        default: Option<Edge>,
    ) -> Result<(Value, bool, Value), OpError> {
        match (
            after,
            before,
            at.or(if after.is_none() && before.is_none() {
                default
            } else {
                None
            }),
        ) {
            (Some(a), None, None) => Ok((Value::String(self.id(a)?), false, Value::Null)),
            (None, Some(b), None) => Ok((Value::String(self.id(b)?), true, Value::Null)),
            (None, None, Some(Edge::Start)) => Ok((Value::Null, true, json!("start"))),
            (None, None, Some(Edge::End)) => Ok((Value::Null, false, json!("end"))),
            _ => Err(self.err(
                "位置は after・before・at（\"start\" / \"end\"）のどれか 1 つで指定してください",
            )),
        }
    }

    fn font(&self, f: &TextFont) -> Result<Value, OpError> {
        let mut m = Map::new();
        if let Some(n) = &f.name {
            m.insert("name".into(), n.clone().into());
        }
        if let Some(s) = f.size {
            if !(1.0..=200.0).contains(&s) {
                return Err(self.err("文字サイズは 1〜200 pt です"));
            }
            m.insert("size".into(), s.into());
        }
        for (k, v) in [
            ("bold", f.bold),
            ("italic", f.italic),
            ("underline", f.underline),
            ("strike", f.strike),
        ] {
            if let Some(v) = v {
                m.insert(k.into(), v.into());
            }
        }
        if let Some(c) = &f.color {
            m.insert("color".into(), self.color(c)?.into());
        }
        if let Some(c) = &f.highlight {
            m.insert("highlight".into(), self.color(c)?.into());
        }
        Ok(Value::Object(m))
    }

    fn para(&self, p: &ParaSpec, m: &mut Map<String, Value>) -> Result<(), OpError> {
        let mm = |v: f64| -> Value { json!(mm100(v)) };
        let pt = |v: f64| -> Value { json!(mm100(v * 25.4 / 72.0)) };
        if let Some(s) = &p.style {
            m.insert("style".into(), s.clone().into());
        }
        if let Some(a) = p.align {
            m.insert("align".into(), align(a).into());
        }
        if let Some(v) = p.indent_left_mm {
            m.insert("indent_left".into(), mm(v));
        }
        if let Some(v) = p.first_line_indent_mm {
            m.insert("indent_first".into(), mm(v));
        }
        if let Some(v) = p.space_before_pt {
            m.insert("space_before".into(), pt(v));
        }
        if let Some(v) = p.space_after_pt {
            m.insert("space_after".into(), pt(v));
        }
        match (p.line_spacing, p.line_height_pt) {
            (Some(_), Some(_)) => {
                return Err(
                    self.err("line_spacing（倍率）と line_height_pt（固定値）はどちらか一方です")
                )
            }
            (Some(x), None) => {
                if !(0.5..=5.0).contains(&x) {
                    return Err(self.err("line_spacing は 0.5〜5.0（倍）です"));
                }
                // com.sun.star.style.LineSpacingMode: PROP=0（％）・FIX=3（1/100 mm）。
                #[allow(clippy::cast_possible_truncation)]
                let pct = (x * 100.0).round() as i64;
                m.insert("line_spacing".into(), json!({"mode": 0, "height": pct}));
            }
            (None, Some(h)) => {
                m.insert(
                    "line_spacing".into(),
                    json!({"mode": 3, "height": mm100(h * 25.4 / 72.0)}),
                );
            }
            (None, None) => {}
        }
        if let Some(v) = p.keep_with_next {
            m.insert("keep_with_next".into(), v.into());
        }
        if let Some(v) = p.page_break_before {
            m.insert("page_break_before".into(), v.into());
        }
        if let Some(f) = &p.font {
            m.insert("font".into(), self.font(f)?);
        }
        Ok(())
    }

    fn header_footer(which: &str, c: Option<&HeaderFooter>) -> Value {
        let content = c.map(|c| {
            let mut pieces = Vec::new();
            let mut rest = c.text.as_str();
            while !rest.is_empty() {
                let next = ["&[Page]", "&[Pages]", "&[Date]", "&[Title]", "&[File]"]
                    .iter()
                    .filter_map(|t| rest.find(t).map(|i| (i, *t)))
                    .min_by_key(|(i, _)| *i);
                if let Some((i, tag)) = next {
                    if i > 0 {
                        pieces.push(json!({"t": "text", "v": &rest[..i]}));
                    }
                    let kind = match tag {
                        "&[Page]" => "page",
                        "&[Pages]" => "pages",
                        "&[Date]" => "date",
                        "&[Title]" => "title",
                        _ => "file",
                    };
                    pieces.push(json!({"t": kind}));
                    rest = &rest[i + tag.len()..];
                } else {
                    pieces.push(json!({"t": "text", "v": rest}));
                    rest = "";
                }
            }
            json!({"pieces": pieces, "align": c.align.map(align), "size": c.size})
        });
        json!({"op": "header_footer", "which": which, "content": content})
    }

    #[allow(clippy::too_many_lines)] // 操作ごとの写像を 1 か所に並べる。
    fn one(&mut self, op: &DocOp) -> Result<Vec<Value>, OpError> {
        Ok(match op {
            DocOp::Insert {
                after,
                before,
                at,
                html,
            } => {
                let (anchor, before, at) =
                    self.position(after.as_ref(), before.as_ref(), *at, None)?;
                vec![
                    json!({"op": "insert_html", "anchor": anchor, "before": before, "at": at, "html": self.html(html)?}),
                ]
            }
            DocOp::Replace { target, html } => {
                let (from, to) = self.target(target)?;
                vec![
                    json!({"op": "replace_html", "from": from, "to": to, "html": self.html(html)?}),
                ]
            }
            DocOp::Delete { target } => {
                let (from, to) = self.target(target)?;
                vec![json!({"op": "delete", "from": from, "to": to})]
            }
            DocOp::ReplaceText {
                find,
                replace,
                within,
                all,
                match_case,
                whole_word,
            } => {
                if find.is_empty() {
                    return Err(self.err("find が空です"));
                }
                let (from, to) = match within {
                    Some(w) => {
                        let (f, t) = self.target(w)?;
                        (Value::String(f), t.map_or(Value::Null, Value::String))
                    }
                    None => (Value::Null, Value::Null),
                };
                vec![
                    json!({"op": "replace_text", "find": find, "replace": replace, "from": from, "to": to,
                            "all": all, "match_case": match_case, "whole_word": whole_word}),
                ]
            }
            DocOp::FormatText {
                find,
                within,
                all,
                match_case,
                font,
            } => {
                if find.is_empty() {
                    return Err(self.err("find が空です"));
                }
                let (from, to) = match within {
                    Some(w) => {
                        let (f, t) = self.target(w)?;
                        (Value::String(f), t.map_or(Value::Null, Value::String))
                    }
                    None => (Value::Null, Value::Null),
                };
                vec![
                    json!({"op": "format_text", "find": find, "from": from, "to": to, "all": all,
                            "match_case": match_case, "font": self.font(font)?}),
                ]
            }
            DocOp::FormatParagraph {
                target,
                para,
                include_tables,
                clear_formatting,
            } => {
                let (from, to) = self.target(target)?;
                let mut m = Map::new();
                m.insert("op".into(), "format_paragraph".into());
                m.insert("from".into(), from.into());
                m.insert("to".into(), to.map_or(Value::Null, Value::String));
                m.insert("include_tables".into(), (*include_tables).into());
                m.insert("clear_formatting".into(), (*clear_formatting).into());
                self.para(para, &mut m)?;
                if m.len() <= 5 && !clear_formatting {
                    return Err(self.err("変える書式がありません"));
                }
                vec![Value::Object(m)]
            }
            DocOp::SetStyle {
                name,
                family,
                based_on,
                para,
            } => {
                if name.trim().is_empty() {
                    return Err(self.err("スタイル名が空です"));
                }
                let mut m = Map::new();
                m.insert("op".into(), "set_style".into());
                m.insert("name".into(), name.clone().into());
                m.insert(
                    "family".into(),
                    match family {
                        StyleFamily::Paragraph => "paragraph",
                        StyleFamily::Character => "character",
                    }
                    .into(),
                );
                if let Some(b) = based_on {
                    m.insert("based_on".into(), b.clone().into());
                }
                if para.style.is_some() {
                    return Err(
                        self.err("set_style に style は書けません（name が対象のスタイルです）")
                    );
                }
                self.para(para, &mut m)?;
                vec![Value::Object(m)]
            }
            DocOp::PageSetup {
                paper,
                orientation,
                margins_mm,
            } => {
                let mut m = Map::new();
                m.insert("op".into(), "page_setup".into());
                if let Some(p) = paper {
                    m.insert("paper".into(), json!(paper_size(*p)));
                }
                if let Some(o) = orientation {
                    m.insert(
                        "landscape".into(),
                        matches!(o, Orientation::Landscape).into(),
                    );
                }
                if let Some(mg) = margins_mm {
                    let all = [mg.top, mg.right, mg.bottom, mg.left];
                    if all.iter().any(|v| !(0.0..=100.0).contains(v)) {
                        return Err(self.err("余白は 0〜100 mm です"));
                    }
                    m.insert("margins".into(), json!(all.map(mm100)));
                }
                vec![Value::Object(m)]
            }
            DocOp::Header { content } => vec![Self::header_footer("header", content.as_ref())],
            DocOp::Footer { content } => vec![Self::header_footer("footer", content.as_ref())],
            DocOp::InsertToc {
                after,
                before,
                at,
                title,
                levels,
            } => {
                let (anchor, before, at) =
                    self.position(after.as_ref(), before.as_ref(), *at, Some(Edge::Start))?;
                if !(1..=10).contains(levels) {
                    return Err(self.err("levels は 1〜10 です"));
                }
                vec![
                    json!({"op": "insert_toc", "anchor": anchor, "before": before, "at": at, "title": title, "levels": levels}),
                ]
            }
            DocOp::UpdateToc {} => vec![json!({"op": "update_indexes"})],
            DocOp::PageBreak { before } => {
                vec![json!({"op": "page_break", "from": self.id(before)?})]
            }
            DocOp::TrackChanges { enabled } => {
                vec![json!({"op": "track_changes", "enabled": enabled})]
            }
            DocOp::InsertRows { .. }
            | DocOp::DeleteRows { .. }
            | DocOp::SetCells { .. }
            | DocOp::FormatCells { .. }
            | DocOp::TableProps { .. }
            | DocOp::StyleTable { .. } => self.table_op(op)?,
            DocOp::TitlePage { enabled } => vec![json!({"op": "title_page", "enabled": enabled})],
            DocOp::AcceptAllChanges {} => vec![json!({"op": "all_changes", "accept": true})],
            DocOp::RejectAllChanges {} => vec![json!({"op": "all_changes", "accept": false})],
        })
    }
}

fn align(a: Align) -> &'static str {
    match a {
        Align::Left => "left",
        Align::Center => "center",
        Align::Right => "right",
        Align::Justify => "justify",
    }
}

fn mm100(mm: f64) -> i64 {
    #[allow(clippy::cast_possible_truncation)]
    let v = (mm * 100.0).round() as i64;
    v
}

fn paper_size(p: Paper) -> [u32; 2] {
    match p {
        Paper::A3 => [29_700, 42_000],
        Paper::A4 => [21_000, 29_700],
        Paper::A5 => [14_800, 21_000],
        Paper::B4 => [25_700, 36_400],
        Paper::B5 => [18_200, 25_700],
        Paper::Letter => [21_590, 27_940],
    }
}

mod table;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn norm(v: &Value) -> DocNormalized {
        normalize_doc_ops(v.as_array().unwrap()).unwrap()
    }

    #[test]
    fn all_targets_the_whole_document_and_bad_ids_explain_the_form() {
        let n = norm(&json!([{"op": "replace", "target": "all", "html": "<p>x</p>"}]));
        assert_eq!(n.ops[0]["from"], "@first");
        assert_eq!(n.ops[0]["to"], "@last");
        assert!(n.block_ids.contains(&"@first".to_string()));
        let err = normalize_doc_ops(&[
            json!({"op": "replace", "target": "b0-15fa60", "html": "<p>x</p>"}),
        ])
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("\"all\"") && err.contains("最初の ID:最後の ID"),
            "{err}"
        );
    }

    #[test]
    fn insert_positions_and_html() {
        let n = norm(&json!([
            {"op": "insert", "after": "b3-00ff", "html": "<h2>見出し</h2><p>本文</p>"},
            {"op": "insert", "at": "start", "html": "<p>x</p>"},
            {"op": "replace", "target": "b5-aaaa:b7-bbbb", "html": "<p>y</p>"}
        ]));
        assert_eq!(n.ops[0]["anchor"], "b3-00ff");
        assert_eq!(n.ops[0]["before"], false);
        assert!(n.ops[0]["html"]
            .as_str()
            .unwrap()
            .contains("<h2>見出し</h2><p>本文</p>"));
        assert_eq!(
            (n.ops[1]["anchor"].clone(), n.ops[1]["at"].clone()),
            (Value::Null, json!("start"))
        );
        assert_eq!(
            (n.ops[2]["from"].clone(), n.ops[2]["to"].clone()),
            (json!("b5-aaaa"), json!("b7-bbbb"))
        );
        assert_eq!(n.block_ids, vec!["b3-00ff", "b5-aaaa", "b7-bbbb"]);
    }

    #[test]
    fn rejects_bad_ids_and_ambiguous_positions() {
        let e = normalize_doc_ops(&[json!({"op": "delete", "target": "段落3"})]).unwrap_err();
        assert!(e.message.contains("ブロック ID"), "{e}");
        let e = normalize_doc_ops(&[
            json!({"op": "insert", "after": "b1-0000", "at": "end", "html": "<p>x</p>"}),
        ])
        .unwrap_err();
        assert!(e.message.contains("どれか 1 つ"), "{e}");
        let e = normalize_doc_ops(&[json!({"op": "insert", "html": "<p>x</p>"})]).unwrap_err();
        assert!(e.message.contains("どれか 1 つ"), "{e}");
    }

    #[test]
    fn paragraph_spec_units() {
        let n = norm(
            &json!([{"op": "format_paragraph", "target": "b2-1234", "align": "justify",
            "first_line_indent_mm": 3.7, "space_after_pt": 6, "line_spacing": 1.5, "font": {"color": "1f3864", "size": 11}}]),
        );
        let v = &n.ops[0];
        assert_eq!(v["align"], "justify");
        assert_eq!(v["indent_first"], 370);
        assert_eq!(v["space_after"], 212);
        assert_eq!(v["line_spacing"], json!({"mode": 0, "height": 150}));
        assert_eq!(v["font"]["color"], "#1F3864");
    }

    #[test]
    fn set_style_and_header_footer() {
        let n = norm(&json!([
            {"op": "set_style", "name": "Heading 1", "font": {"color": "#1F3864", "size": 16}, "space_before_pt": 12},
            {"op": "footer", "content": {"text": "- &[Page] / &[Pages] -", "align": "center"}},
            {"op": "header", "content": null}
        ]));
        assert_eq!(n.ops[0]["family"], "paragraph");
        assert_eq!(n.ops[0]["font"]["size"], 16.0);
        assert_eq!(
            n.ops[1]["content"]["pieces"],
            json!([{"t": "text", "v": "- "}, {"t": "page"}, {"t": "text", "v": " / "}, {"t": "pages"}, {"t": "text", "v": " -"}])
        );
        assert_eq!(n.ops[2]["content"], Value::Null);
    }

    #[test]
    fn unknown_fields_point_at_the_op() {
        let e = normalize_doc_ops(&[
            json!({"op": "format_paragraph", "target": "b1-0000", "colour": "red"}),
        ])
        .unwrap_err();
        assert_eq!((e.index, e.op), (1, "format_paragraph"));
    }
}
