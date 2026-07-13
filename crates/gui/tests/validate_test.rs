//! UI スペック検証層の検証マトリクス（Task 6.2/6.3 受け入れ条件・純粋・依存なし）。

#![allow(
    clippy::pedantic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use gui::validate::{limits, validate_spec};
use serde_json::json;

/// 検証エラーのコード一覧を取り出すヘルパ。
fn error_codes(raw: serde_json::Value) -> Vec<String> {
    match validate_spec(&raw) {
        Ok(_) => Vec::new(),
        Err(errors) => errors.into_iter().map(|e| e.code).collect(),
    }
}

fn assert_rejected_with(raw: serde_json::Value, code: &str) {
    let codes = error_codes(raw);
    assert!(
        codes.iter().any(|c| c == code),
        "expected code {code}, got {codes:?}"
    );
}

/// 最小の妥当なスペック。
fn minimal(root: serde_json::Value) -> serde_json::Value {
    json!({ "version": 1, "root": root })
}

#[test]
fn accepts_catalog_components_with_typed_props() {
    // 6.2 受け入れ条件: カタログの各コンポーネントが型付き props で表現できる。
    let spec = json!({
        "version": 1,
        "actions": [
            { "type": "handler", "id": "submit", "handler": "chat.submit" },
            { "type": "tool", "id": "search", "tool": "doc_search" }
        ],
        "root": {
            "component": "container",
            "title": "デモ",
            "layout": "vertical",
            "children": [
                { "component": "text", "text": "説明\n2行目", "variant": "heading" },
                { "component": "link", "text": "docs", "href": "https://example.com/docs" },
                { "component": "button", "label": "検索", "on_click": { "action": "search" } },
                {
                    "component": "form",
                    "id": "f1",
                    "submit": { "action": "submit" },
                    "fields": [
                        { "component": "text_input", "id": "comment", "label": "コメント", "multiline": true },
                        { "component": "select", "id": "rate", "label": "評価",
                          "options": [ {"value": "1", "label": "低"}, {"value": "5", "label": "高"} ],
                          "default": "5" }
                    ]
                },
                {
                    "component": "table",
                    "columns": [ {"label": "項目"}, {"label": "値", "align": "right"} ],
                    "rows": [ ["A", 1.0], ["B", true] ]
                },
                {
                    "component": "chart", "kind": "line",
                    "data": [ {"x": "1月", "y": 1.0}, {"x": "2月", "y": 2.5, "series": "s1"} ]
                }
            ]
        }
    });
    let doc = validate_spec(&spec).expect("valid spec");
    assert_eq!(doc.version, 1);
    assert_eq!(doc.actions.len(), 2);
}

#[test]
fn rejects_unknown_component_and_raw_html() {
    // 6.2/6.3: カタログ外コンポーネント・生 HTML はスキーマ上表現不可能＝拒否。
    assert_rejected_with(
        minimal(json!({ "component": "iframe", "src": "https://evil.example" })),
        "gui.unknown_component",
    );
    assert_rejected_with(
        minimal(json!({ "component": "html", "html": "<script>alert(1)</script>" })),
        "gui.unknown_component",
    );
}

#[test]
fn rejects_unknown_props_and_inline_code() {
    // 未知 props（イベントハンドラ等のインラインコード持ち込み口）は拒否。
    assert_rejected_with(
        minimal(json!({ "component": "text", "text": "x", "onclick": "alert(1)" })),
        "gui.unknown_prop",
    );
    assert_rejected_with(
        minimal(json!({
            "component": "button", "label": "x",
            "on_click": { "action": "a", "handler_inline": "fetch('https://x')" }
        })),
        "gui.unknown_prop",
    );
}

#[test]
fn rejects_unknown_action_ref() {
    // 6.3 受け入れ条件: 存在しないアクション ID を参照するスペックは拒否。
    assert_rejected_with(
        minimal(json!({
            "component": "button", "label": "x", "on_click": { "action": "missing" }
        })),
        "gui.unknown_action_ref",
    );
}

#[test]
fn rejects_reserved_components() {
    assert_rejected_with(
        minimal(json!({ "component": "map" })),
        "gui.component_unavailable",
    );
    assert_rejected_with(
        minimal(json!({ "component": "image" })),
        "gui.component_unavailable",
    );
}

#[test]
fn rejects_forbidden_url_schemes() {
    for href in [
        "javascript:alert(1)",
        "data:text/html;base64,PGI+",
        "http://insecure.example",
        "/relative/path",
    ] {
        assert_rejected_with(
            minimal(json!({ "component": "link", "text": "x", "href": href })),
            "gui.forbidden_url_scheme",
        );
    }
}

#[test]
fn rejects_destructive_tool_bindings() {
    // 破壊系ツール（shell 等）は UI アクションに束縛できない（保存時 fail-closed）。
    for tool in [
        "shell",
        "fs_delete",
        "fs_write",
        "emit_ui",
        "code_interpreter",
    ] {
        let spec = json!({
            "version": 1,
            "actions": [ { "type": "tool", "id": "a", "tool": tool } ],
            "root": { "component": "button", "label": "x", "on_click": { "action": "a" } }
        });
        assert_rejected_with(spec, "gui.action_tool_forbidden");
    }
}

#[test]
fn rejects_unknown_tool_name_structurally() {
    // 閉語彙外のツール名はスキーマ違反（ToolName enum に存在しない）。
    let spec = json!({
        "version": 1,
        "actions": [ { "type": "tool", "id": "a", "tool": "rm_rf" } ],
        "root": { "component": "button", "label": "x", "on_click": { "action": "a" } }
    });
    assert_rejected_with(spec, "gui.unknown_component");
}

#[test]
fn rejects_depth_and_node_overflow() {
    // 深さ超過。
    let mut node = json!({ "component": "text", "text": "leaf" });
    for _ in 0..limits::MAX_DEPTH {
        node = json!({ "component": "container", "children": [node] });
    }
    assert_rejected_with(minimal(node), "gui.too_deep");

    // ノード数超過（1 階層に大量の子）。
    let children: Vec<serde_json::Value> = (0..=limits::MAX_NODES)
        .map(|i| json!({ "component": "text", "text": format!("t{i}") }))
        .collect();
    let spec = minimal(json!({ "component": "container", "children": children }));
    let codes = error_codes(spec);
    assert!(codes
        .iter()
        .any(|c| c == "gui.too_many_nodes" || c == "gui.too_many_children"));
}

#[test]
fn rejects_table_row_mismatch_and_string_limits() {
    assert_rejected_with(
        minimal(json!({
            "component": "table",
            "columns": [ {"label": "a"}, {"label": "b"} ],
            "rows": [ ["only-one"] ]
        })),
        "gui.table_row_mismatch",
    );
    assert_rejected_with(
        minimal(json!({
            "component": "text",
            "text": "x".repeat(limits::MAX_TEXT_CHARS + 1)
        })),
        "gui.string_too_long",
    );
    // 制御文字（エスケープシーケンス注入）は補正でなく拒否。
    assert_rejected_with(
        minimal(json!({ "component": "text", "text": "bad\u{1b}[31mred" })),
        "gui.control_char",
    );
}

#[test]
fn rejects_duplicate_and_invalid_ids() {
    let spec = json!({
        "version": 1,
        "actions": [
            { "type": "handler", "id": "a", "handler": "chat.submit" },
            { "type": "tool", "id": "a", "tool": "doc_search" }
        ],
        "root": { "component": "button", "label": "x", "on_click": { "action": "a" } }
    });
    assert_rejected_with(spec, "gui.duplicate_action_id");

    let spec = json!({
        "version": 1,
        "actions": [ { "type": "handler", "id": "変な id!", "handler": "chat.submit" } ],
        "root": { "component": "text", "text": "x" }
    });
    assert_rejected_with(spec, "gui.invalid_id");
}

#[test]
fn rejects_unsupported_version_and_collects_all_errors() {
    // 複数違反は全件収集される（最初の 1 件で止めない）。
    let spec = json!({
        "version": 2,
        "actions": [ { "type": "tool", "id": "a", "tool": "shell" } ],
        "root": { "component": "link", "text": "x", "href": "javascript:x" }
    });
    let codes = error_codes(spec);
    assert!(codes.contains(&"gui.unsupported_version".to_string()));
    assert!(codes.contains(&"gui.action_tool_forbidden".to_string()));
    assert!(codes.contains(&"gui.forbidden_url_scheme".to_string()));
}

#[test]
fn rejects_select_default_not_in_options() {
    let spec = json!({
        "version": 1,
        "actions": [ { "type": "handler", "id": "s", "handler": "chat.submit" } ],
        "root": {
            "component": "form", "id": "f", "submit": { "action": "s" },
            "fields": [
                { "component": "select", "id": "x", "label": "x",
                  "options": [ {"value": "1", "label": "a"} ], "default": "9" }
            ]
        }
    });
    assert_rejected_with(spec, "gui.invalid_default");
}

#[test]
fn oversized_spec_is_rejected() {
    let spec = minimal(json!({
        "component": "text",
        "text": "y".repeat(limits::MAX_SPEC_BYTES + 1)
    }));
    assert_rejected_with(spec, "gui.spec_too_large");
}

#[test]
fn accepts_extended_chart_kinds_and_flags() {
    // 拡張チャート種（PR1）: donut/scatter/radar/radial_bar/combo/funnel/treemap ＋ stacked/line_series/xv。
    for kind in [
        "bar",
        "line",
        "area",
        "pie",
        "donut",
        "scatter",
        "radar",
        "radial_bar",
        "combo",
        "funnel",
        "treemap",
    ] {
        let spec = minimal(json!({
            "component": "chart", "kind": kind, "title": "t",
            "stacked": true,
            "line_series": ["目標"],
            "data": [
                { "x": "1月", "y": 1.0, "series": "実績", "xv": 1.0 },
                { "x": "2月", "y": 2.5, "series": "目標", "xv": 2.0 }
            ]
        }));
        assert!(
            validate_spec(&spec).is_ok(),
            "kind {kind} should validate: {:?}",
            error_codes(spec)
        );
    }
}

#[test]
fn rejects_overlong_line_series_label() {
    // line_series の各系列名はラベル上限を超えると拒否（NaN/Inf は JSON に載らないため
    // y/xv の有限数チェックは型の段で担保され、ここでは文字列上限を突く）。
    assert_rejected_with(
        minimal(json!({
            "component": "chart", "kind": "combo",
            "data": [ { "x": "a", "y": 1.0, "xv": 2.0 } ],
            "line_series": [ "x".repeat(limits::MAX_LABEL_CHARS + 1) ]
        })),
        "gui.string_too_long",
    );
}

#[test]
fn rejects_negative_values_for_magnitude_charts() {
    // 面積/割合で大小を表す種別は負値を拒否（bar/line 等は許容）。
    for kind in ["pie", "donut", "funnel", "treemap", "radial_bar"] {
        assert_rejected_with(
            minimal(json!({
                "component": "chart", "kind": kind,
                "data": [ { "x": "A", "y": 5.0 }, { "x": "B", "y": -2.0 } ]
            })),
            "gui.negative_not_allowed",
        );
    }
    // bar は負値可（前年差分など）。
    let ok = minimal(json!({
        "component": "chart", "kind": "bar",
        "data": [ { "x": "A", "y": -2.0 } ]
    }));
    assert!(validate_spec(&ok).is_ok(), "{:?}", error_codes(ok));
}

#[test]
fn accepts_rich_input_fields() {
    // PR3: checkbox/radio/date/slider/rating ＋ allow_other。
    let spec = json!({
        "version": 1,
        "actions": [ { "type": "handler", "id": "s", "handler": "chat.submit" } ],
        "root": {
            "component": "form", "id": "f", "submit": { "action": "s" },
            "fields": [
                { "component": "checkbox", "id": "c", "label": "好み",
                  "options": [ {"value": "1", "label": "A"} ], "default": ["1"], "allow_other": true },
                { "component": "radio", "id": "r", "label": "評価",
                  "options": [ {"value": "1", "label": "低"} ], "default": "1" },
                { "component": "date", "id": "d", "label": "期間", "range": true },
                { "component": "slider", "id": "sl", "label": "量", "min": 0.0, "max": 10.0, "step": 1.0, "default": 5.0 },
                { "component": "rating", "id": "rt", "label": "満足度", "max": 5, "default": 4 }
            ]
        }
    });
    assert!(validate_spec(&spec).is_ok(), "{:?}", error_codes(spec));
}

#[test]
fn rejects_invalid_slider_and_rating() {
    let bad_slider = json!({
        "version": 1,
        "actions": [ { "type": "handler", "id": "s", "handler": "chat.submit" } ],
        "root": { "component": "form", "id": "f", "submit": { "action": "s" },
            "fields": [ { "component": "slider", "id": "sl", "label": "x", "min": 10.0, "max": 1.0 } ] }
    });
    assert_rejected_with(bad_slider, "gui.invalid_range");

    let bad_rating = json!({
        "version": 1,
        "actions": [ { "type": "handler", "id": "s", "handler": "chat.submit" } ],
        "root": { "component": "form", "id": "f", "submit": { "action": "s" },
            "fields": [ { "component": "rating", "id": "rt", "label": "x", "max": 3, "default": 9 } ] }
    });
    assert_rejected_with(bad_rating, "gui.invalid_default");
}

#[test]
fn accepts_layout_components_with_nesting() {
    // PR2: callout/accordion/tabs/stepper/badge_list/key_value/code_block。
    let spec = json!({
        "version": 1,
        "root": {
            "component": "container",
            "children": [
                { "component": "callout", "tone": "warning", "title": "注意", "text": "在庫僅少" },
                { "component": "accordion", "items": [
                    { "title": "詳細", "open": true, "children": [ { "component": "text", "text": "本文" } ] }
                ] },
                { "component": "tabs", "tabs": [
                    { "label": "A", "children": [ { "component": "text", "text": "a" } ] },
                    { "label": "B", "children": [ { "component": "badge_list", "badges": [ {"label": "x"} ] } ] }
                ] },
                { "component": "stepper", "steps": [
                    { "title": "S1", "status": "done" }, { "title": "S2", "status": "doing" }
                ] },
                { "component": "badge_list", "badges": [ {"label": "tag", "tone": "info"} ] },
                { "component": "key_value", "title": "詳細", "items": [ {"key": "k", "value": "v"} ] },
                { "component": "code_block", "code": "let x = 1;", "language": "rust" }
            ]
        }
    });
    assert!(validate_spec(&spec).is_ok(), "{:?}", error_codes(spec));
}

#[test]
fn rejects_bad_child_inside_accordion() {
    // ネストした子ツリーも走査検証される（カタログ外は拒否）。
    assert_rejected_with(
        minimal(json!({
            "component": "accordion",
            "items": [ { "title": "t", "children": [ { "component": "iframe", "src": "https://x" } ] } ]
        })),
        "gui.unknown_component",
    );
}

#[test]
fn accepts_stat_tile() {
    let spec = minimal(json!({
        "component": "stat",
        "label": "今月の売上", "value": "¥1.2M", "unit": "円",
        "delta": 12.4, "delta_label": "前月比",
        "trend": [1.0, 2.0, 1.5, 3.0], "caption": "順調"
    }));
    assert!(validate_spec(&spec).is_ok(), "{:?}", error_codes(spec));
}

#[test]
fn rejects_stat_with_too_many_trend_points() {
    let spec = minimal(json!({
        "component": "stat", "label": "l", "value": "v",
        "trend": vec![1.0_f64; limits::MAX_SPARKLINE_POINTS + 1]
    }));
    assert_rejected_with(spec, "gui.too_many_points");
}
