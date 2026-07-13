//! 決定的スタブの固定 UI スペック（`genui:` 駆動）。
//!
//! [`super::stub`] から分離した generative UI のフィクスチャ群（gui クレートの検証を通る形・
//! `bad` のみ意図的に不正）。ファイルサイズと関心の分離のため独立させる。

use serde_json::{json, Value};

/// `genui:` 駆動の固定 UI スペックを組む。
///
/// - `table` / `stat`・`chart`（bar）／`chart:<kind>`（種別指定・未知値は bar フォールバック）
/// - `bad`: カタログ外コンポーネント（検証拒否→テキストフォールバックの決定的検証用）
/// - `workflow <name>`: 名前参照のワークフロー起動ボタン（検証時に version がピンされる）
/// - 既定: chat.submit 束縛つきフォーム
pub(super) fn genui_spec(kind: &str) -> Value {
    if let Some(name) = kind.strip_prefix("workflow") {
        let name = name.trim();
        return json!({
            "version": 1,
            "actions": [
                { "type": "workflow", "id": "run", "workflow": { "name": name } }
            ],
            "root": {
                "component": "container",
                "title": "ワークフロー実行",
                "children": [
                    { "component": "button", "label": "実行", "on_click": { "action": "run" } }
                ]
            }
        });
    }
    match kind {
        "table" => json!({
            "version": 1,
            "root": {
                "component": "table",
                "title": "サンプル表",
                "columns": [ { "label": "項目" }, { "label": "値", "align": "right" } ],
                "rows": [ ["A", 1.0], ["B", 2.0] ]
            }
        }),
        // `chart` は bar、`chart:<kind>` は種別指定（scatter/radar/... の決定的描画）。
        // 未知の接尾辞はそのまま渡すと検証で拒否されるため、閉語彙でホワイトリスト化し
        // 未知値は bar にフォールバックして常に検証を通る固定スペックにする。
        k if k == "chart" || k.starts_with("chart:") => {
            const KNOWN_KINDS: &[&str] = &[
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
            ];
            let requested = k.strip_prefix("chart:").unwrap_or("bar").trim();
            let chart_kind = if KNOWN_KINDS.contains(&requested) {
                requested
            } else {
                "bar"
            };
            json!({
                "version": 1,
                "root": {
                    "component": "chart",
                    "kind": chart_kind,
                    "title": "月次売上",
                    "stacked": chart_kind == "area" || chart_kind == "bar",
                    "line_series": ["目標"],
                    "data": [
                        { "x": "1月", "y": 10.0, "series": "実績", "xv": 1.0 },
                        { "x": "2月", "y": 20.0, "series": "実績", "xv": 2.0 },
                        { "x": "3月", "y": 16.0, "series": "実績", "xv": 3.0 },
                        { "x": "1月", "y": 12.0, "series": "目標", "xv": 1.0 },
                        { "x": "2月", "y": 18.0, "series": "目標", "xv": 2.0 },
                        { "x": "3月", "y": 22.0, "series": "目標", "xv": 3.0 }
                    ]
                }
            })
        }
        "stat" => json!({
            "version": 1,
            "root": {
                "component": "stat",
                "label": "今月の売上",
                "value": "¥1.28M",
                "delta": 12.4,
                "delta_label": "前月比",
                "trend": [8.0, 9.5, 9.0, 11.0, 10.5, 12.8],
                "caption": "目標達成"
            }
        }),
        // カタログ外コンポーネント（検証拒否→テキストフォールバックの決定的検証用）。
        "bad" => json!({
            "version": 1,
            "root": { "component": "iframe", "src": "https://evil.example" }
        }),
        // 既定はフォーム（chat.submit 束縛）。
        _ => json!({
            "version": 1,
            "actions": [
                { "type": "handler", "id": "submit", "handler": "chat.submit" }
            ],
            "root": {
                "component": "form",
                "id": "feedback",
                "title": "フィードバック",
                "submit": { "action": "submit" },
                "submit_label": "送信",
                "fields": [
                    { "component": "text_input", "id": "comment", "label": "コメント", "required": true }
                ]
            }
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn genui_chart_kind_is_parametrized() {
        // `genui:chart:<kind>` は kind を反映し、`chart`/`chart:` は既定 bar。
        assert_eq!(genui_spec("chart:radar")["root"]["kind"], "radar");
        assert_eq!(genui_spec("chart:scatter")["root"]["kind"], "scatter");
        assert_eq!(genui_spec("chart")["root"]["kind"], "bar");
        assert_eq!(genui_spec("chart:")["root"]["kind"], "bar");
        assert_eq!(genui_spec("chart:bar")["root"]["component"], "chart");
        // 未知の kind は bar にフォールバック（常に検証を通る固定スペック）。
        assert_eq!(genui_spec("chart:unknown")["root"]["kind"], "bar");
    }

    #[test]
    fn genui_stat_spec_shape() {
        let spec = genui_spec("stat");
        assert_eq!(spec["root"]["component"], "stat");
        assert!(!spec["root"]["trend"].as_array().unwrap().is_empty());
    }
}
