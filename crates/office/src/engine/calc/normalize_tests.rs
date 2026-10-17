#![allow(clippy::unwrap_used, clippy::needless_pass_by_value)]

use serde_json::json;

use super::*;

fn ops(v: serde_json::Value) -> Vec<SheetOp> {
    serde_json::from_value(v).unwrap()
}

fn one(v: serde_json::Value) -> Value {
    let n = normalize(&ops(json!([v])), None).unwrap();
    assert_eq!(n.ops.len(), 1);
    n.ops[0].clone()
}

fn fails(v: serde_json::Value) -> String {
    normalize(&ops(json!([v])), None).unwrap_err().to_string()
}

#[test]
fn set_values_from_anchor_infers_size_and_converts_formulas() {
    let v = one(
        json!({"op": "set_values", "range": "売上!B2", "values": [["東京", 1200, "=SUM(C2:D2)"], [null, true, {"date": "2026-04-01"}]]}),
    );
    assert_eq!(v["sheet"], "売上");
    assert_eq!(v["area"], json!([1, 1, 3, 2]));
    assert_eq!(v["values"][0][0], json!({"t": "s", "v": "東京"}));
    assert_eq!(v["values"][0][1], json!({"t": "n", "v": 1200.0}));
    assert_eq!(v["values"][0][2], json!({"t": "f", "v": "=SUM(C2:D2)"}));
    assert_eq!(v["values"][1][0], Value::Null);
    assert_eq!(v["values"][1][1], json!({"t": "b", "v": true}));
    assert_eq!(
        v["values"][1][2],
        json!({"t": "d", "v": 46113.0, "fmt": "yyyy/mm/dd"})
    );
}

#[test]
fn literal_text_is_not_a_formula() {
    let v =
        one(json!({"op": "set_values", "range": "A1", "values": [[{"text": "=はじめに"}, "="]]}));
    assert_eq!(v["values"][0][0], json!({"t": "s", "v": "=はじめに"}));
    assert_eq!(v["values"][0][1], json!({"t": "s", "v": "="}));
}

#[test]
fn set_values_rejects_size_mismatch() {
    let e = fails(json!({"op": "set_values", "range": "A1:B2", "values": [[1, 2, 3]]}));
    assert!(e.contains("1 件目（set_values）"), "{e}");
    assert!(e.contains("2×2"), "{e}");
}

#[test]
fn fill_formula_shifts_relative_refs_per_cell() {
    let v = one(json!({"op": "fill_formula", "range": "E2:E4", "formula": "=SUM(B2:D2)/$B$1"}));
    assert_eq!(v["op"], "set_values");
    assert_eq!(v["values"][0][0]["v"], "=SUM(B2:D2)/$B$1");
    assert_eq!(v["values"][2][0]["v"], "=SUM(B4:D4)/$B$1");
}

#[test]
fn format_maps_font_fill_align_and_borders() {
    let v = one(json!({
        "op": "format", "range": ["A1:E1", "A5:E5"], "number_format": "#,##0",
        "font": {"bold": true, "color": "1f3864", "size": 12},
        "fill": null, "align": {"horizontal": "center", "vertical": "center", "wrap": true},
        "border": {"all": "thin", "bottom": {"style": "double", "color": "#000000"}}
    }));
    assert_eq!(v["areas"], json!([[0, 0, 4, 0], [0, 4, 4, 4]]));
    assert_eq!(
        v["font"],
        json!({"bold": true, "color": "#1F3864", "size": 12.0})
    );
    assert_eq!(v["fill"], Value::Null);
    assert_eq!(
        v["align"],
        json!({"horizontal": "center", "vertical": "center", "wrap": true})
    );
    // 線の値は「Excel の罫線を読み込んだときと同じ」（xlsx へ書き出して線種が保たれる値）。
    let thin =
        json!({"style": 0, "width": 26, "outer": 26, "inner": 0, "dist": 0, "color": "#000000"});
    assert_eq!(v["borders"]["outline"], thin);
    assert_eq!(v["borders"]["inner"], thin);
    assert_eq!(
        v["borders"]["bottom"],
        json!({"style": 15, "width": 62, "outer": 18, "inner": 18, "dist": 26, "color": "#000000"})
    );
}

#[test]
fn format_without_changes_or_with_bad_color_is_rejected() {
    assert!(fails(json!({"op": "format", "range": "A1"})).contains("変える書式がありません"));
    assert!(fails(json!({"op": "format", "range": "A1", "fill": "red"})).contains("#RRGGBB"));
    assert!(
        fails(json!({"op": "format", "range": ["A1", "Sheet2!B1"], "fill": "#FFFFFF"}))
            .contains("同じシート")
    );
}

#[test]
fn widths_heights_and_rows_columns() {
    let v = one(json!({"op": "column_width", "columns": "B:D", "width": 8.43}));
    assert_eq!(
        (v["from"].clone(), v["to"].clone(), v["width"].clone()),
        (json!(1), json!(3), json!(1693))
    );
    assert_eq!(
        one(json!({"op": "column_width", "columns": "A", "width": "auto"}))["width"],
        Value::Null
    );
    assert_eq!(
        one(json!({"op": "row_height", "rows": "1:2", "height": 24}))["height"],
        json!(847)
    );
    let v = one(json!({"op": "insert_rows", "before": 5, "count": 2}));
    assert_eq!((v["at"].clone(), v["count"].clone()), (json!(4), json!(2)));
    let v = one(json!({"op": "delete_columns", "columns": "C:D"}));
    assert_eq!((v["at"].clone(), v["count"].clone()), (json!(2), json!(2)));
    assert!(
        fails(json!({"op": "column_width", "columns": "A1", "width": 10})).contains("列を指定")
    );
}

#[test]
fn sort_keys_are_relative_to_range() {
    let v = one(json!({"op": "sort", "range": "B2:F20", "by": [{"column": "E", "order": "desc"}]}));
    assert_eq!(v["keys"], json!([{"field": 3, "ascending": false}]));
    assert!(
        fails(json!({"op": "sort", "range": "B2:F20", "by": [{"column": "A"}]}))
            .contains("範囲 B2:F20 の外")
    );
}

#[test]
fn conditional_format_compare_and_formula() {
    let v = one(
        json!({"op": "conditional_format", "range": "B2:D4", "when": {"operator": "<", "value": 600}, "style": {"font_color": "#C00000", "fill": "#FFC7CE"}}),
    );
    assert_eq!(
        (v["operator"].clone(), v["formula1"].clone()),
        (json!("LESS"), json!("600"))
    );
    let v = one(
        json!({"op": "conditional_format", "range": "A2:E20", "when": {"formula": "=$E2>1000"}, "style": {"bold": true}}),
    );
    assert_eq!(
        (v["operator"].clone(), v["formula1"].clone()),
        (json!("FORMULA"), json!("$E2>1000"))
    );
    let v = one(
        json!({"op": "conditional_format", "range": "C2:C9", "when": {"operator": "=", "value": "完了"}, "style": {"fill": "#E2EFDA"}}),
    );
    assert_eq!(v["formula1"], json!("\"完了\""));
    assert!(fails(json!({"op": "conditional_format", "range": "A1", "when": {"operator": "between", "value": 1}, "style": {"bold": true}})).contains("value2"));
}

#[test]
fn data_validation_list_and_number() {
    let v = one(
        json!({"op": "data_validation", "range": "C2:C100", "rule": {"type": "list", "items": ["未着手", "完了"]}, "error_message": "一覧から選んでください"}),
    );
    assert_eq!(
        (v["type"].clone(), v["formula1"].clone()),
        (json!("LIST"), json!("\"未着手\";\"完了\""))
    );
    let v = one(
        json!({"op": "data_validation", "range": "D2:D9", "rule": {"type": "list", "source": "マスタ!A2:A20"}}),
    );
    assert_eq!(v["formula1"], json!("$マスタ.$A$2:$A$20"));
    let v = one(
        json!({"op": "data_validation", "range": "E2:E9", "rule": {"type": "whole_number", "operator": "between", "value": 0, "value2": 100}}),
    );
    assert_eq!(
        (
            v["type"].clone(),
            v["operator"].clone(),
            v["formula2"].clone()
        ),
        (json!("WHOLE"), json!("BETWEEN"), json!("100"))
    );
}

#[test]
fn chart_defaults_and_ranges() {
    let v = one(
        json!({"op": "add_chart", "name": "推移", "type": "line", "data": "A1:C13", "anchor": "集計!E2", "title": "月次"}),
    );
    assert_eq!(v["sheet"], "集計");
    assert_eq!(v["ranges"], json!([["集計", 0, 0, 2, 12]]));
    assert_eq!(v["size"], json!([15000, 9000]));
    assert_eq!(v["anchor"], json!([4, 1]));
    assert_eq!(v["first_row_header"], json!(true));
}

#[test]
fn page_setup_only_carries_given_fields() {
    let v = one(
        json!({"op": "page_setup", "orientation": "landscape", "paper": "A4", "fit_to": {"width": 1, "height": 0}, "print_area": "A1:H40", "repeat_rows": "1:2"}),
    );
    assert_eq!(v["landscape"], json!(true));
    assert_eq!(v["paper"], json!([21000, 29700]));
    assert_eq!(v["fit"], json!([1, 0]));
    assert_eq!(v["print_area"], json!([0, 0, 7, 39]));
    assert_eq!(v["repeat_rows"], json!([0, 1]));
    assert!(v.get("margins").is_none());
    let v = one(json!({"op": "page_setup", "print_area": null}));
    assert_eq!(v["print_area"], Value::Null);
}

#[test]
fn names_and_sheets_are_validated() {
    assert!(
        fails(json!({"op": "define_name", "name": "A1", "range": "A1:B2"})).contains("使えません")
    );
    assert!(fails(json!({"op": "add_sheet", "name": "a/b"})).contains("使えません"));
    let v = one(json!({"op": "define_name", "name": "売上表", "range": "売上!A1:E5"}));
    assert_eq!(
        (v["sheet"].clone(), v["area"].clone()),
        (json!("売上"), json!([0, 0, 4, 4]))
    );
}

#[test]
fn default_sheet_applies_when_range_has_none() {
    let n = normalize(
        &ops(json!([{"op": "merge", "range": "A1:C1"}])),
        Some("報告"),
    )
    .unwrap();
    assert_eq!(n.ops[0]["sheet"], "報告");
}

#[test]
fn rejects_unknown_fields_and_ops() {
    assert!(serde_json::from_value::<Vec<SheetOp>>(
        json!([{"op": "merge", "range": "A1:B1", "colour": "red"}])
    )
    .is_err());
    assert!(serde_json::from_value::<Vec<SheetOp>>(json!([{"op": "explode"}])).is_err());
}

#[test]
fn units_round_trip() {
    assert_eq!(chars_to_mm100(8.43), 1693);
    assert!((mm100_to_chars(1693) - 8.4).abs() < 0.05);
    assert_eq!(pt_to_mm100(72.0), 2540);
    assert_eq!(date_serial("2026/04/01 12:00").unwrap(), (46113.5, true));
    assert_eq!(normalize_color("#abcdef").as_deref(), Some("#ABCDEF"));
    assert_eq!(normalize_color("#abc"), None);
}

#[test]
fn parse_ops_points_at_the_bad_op_and_accepts_per_op_sheet() {
    use super::super::parse::parse_ops;
    let err = parse_ops(&[
        json!({"op": "merge", "range": "A1:B1"}),
        json!({"op": "set_values", "range": "A1", "values": ["=SUM(A1:A3)"]}),
    ])
    .unwrap_err();
    assert_eq!((err.index, err.op), (2, "set_values"));
    assert!(err.message.contains("2 次元"), "{}", err.message);

    let parsed = parse_ops(&[
        json!({"op": "format", "sheet": "集計", "range": "A1", "fill": "#FFFFFF"}),
        json!({"op": "rename_sheet", "sheet": "Sheet1", "name": "売上"}),
    ])
    .unwrap();
    assert_eq!(
        parsed[0].1.as_deref(),
        Some("集計"),
        "操作に書いた sheet はその操作の既定"
    );
    assert_eq!(
        parsed[1].1, None,
        "シートを対象にする操作の sheet は対象名のまま"
    );
    let items: Vec<_> = parsed.iter().map(|(o, s)| (o, s.as_deref())).collect();
    let n = normalize_with_sheets(&items, None).unwrap();
    assert_eq!(n.ops[0]["sheet"], "集計");
}

#[test]
fn rename_is_followed_by_later_ops_and_the_default_sheet() {
    let n = normalize(
        &ops(json!([
            {"op": "rename_sheet", "sheet": "Sheet1", "name": "見積書"},
            {"op": "merge", "range": "A1:C1"},
            {"op": "format", "range": "Sheet1!A2", "fill": "#FFFFFF"}
        ])),
        Some("Sheet1"),
    )
    .unwrap();
    assert_eq!(n.ops[0]["sheet"], "Sheet1");
    assert_eq!(n.ops[1]["sheet"], "見積書", "既定シートも新しい名前へ");
    assert_eq!(n.ops[2]["sheet"], "見積書", "旧名で書いても読み替える");
}

#[test]
fn style_table_expands_into_header_bands_total_and_borders() {
    let n = normalize(
        &ops(json!([{"op": "style_table", "range": "A1:E7", "theme": "navy", "total_row": true}])),
        None,
    )
    .unwrap();
    assert_eq!(n.ops.len(), 5, "{:?}", n.ops);
    assert_eq!(n.ops[0]["borders"]["outline"]["width"], 26);
    assert_eq!(n.ops[1]["areas"], json!([[0, 0, 4, 0]]));
    assert_eq!(n.ops[1]["fill"], "#1F3864");
    assert_eq!(n.ops[1]["font"]["color"], "#FFFFFF");
    assert_eq!(
        n.ops[2]["areas"],
        json!([[0, 2, 4, 2], [0, 4, 4, 4]]),
        "見出しの次から 1 行おき（合計行は除く）"
    );
    assert_eq!(n.ops[3]["areas"], json!([[0, 6, 4, 6]]));
    assert_eq!(n.ops[4]["borders"]["top"]["style"], 15);
    assert_eq!(n.touched.len(), 1);
}

#[test]
fn style_table_with_several_total_rows() {
    let n = normalize(
        &ops(json!([{"op": "style_table", "range": "A1:F9", "total_rows": 2, "banded": false}])),
        None,
    )
    .unwrap();
    // 罫線・見出し・集計行（2 行）・集計行の上の二重線。
    assert_eq!(n.ops.len(), 4, "{:?}", n.ops);
    assert_eq!(n.ops[2]["areas"], json!([[0, 7, 5, 8]]));
    assert_eq!(n.ops[3]["areas"], json!([[0, 7, 5, 7]]));
}

#[test]
fn page_setup_header_footer_fields() {
    let v = one(
        json!({"op": "page_setup", "footer": {"center": "&[Page] / &[Pages] ページ", "right": "&[Date]"}, "header": null}),
    );
    assert_eq!(v["header"], Value::Null);
    assert_eq!(
        v["footer"]["center"],
        json!([{"t": "page"}, {"t": "text", "v": " / "}, {"t": "pages"}, {"t": "text", "v": " ページ"}])
    );
    assert_eq!(v["footer"]["right"], json!([{"t": "date"}]));
    assert_eq!(v["footer"]["left"], json!([]));
}
