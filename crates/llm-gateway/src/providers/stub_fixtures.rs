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
        // 質問カード（Claude Code 風・複数質問＋自由記述・回答は chat.submit へ）。
        "question" => question_spec(),
        // 地図（マーカー＋ルート・座標のみ／タイルはサーバ設定・PR5）。
        "map" => map_spec(),
        // ドメインカード（RAG 引用元・旅程・天気・比較・タイムライン・PR6）。
        "domain" => domain_spec(),
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

/// 質問カード（Claude Code の AskUserQuestion 相当）の固定スペック。
/// `genui_spec` の肥大化を避けて分離する。短い選択式・複数選択・自由記述を混在させる。
fn question_spec() -> Value {
    json!({
        "version": 1,
        "actions": [
            { "type": "handler", "id": "answer", "handler": "chat.submit" }
        ],
        "root": {
            "component": "question_card",
            "id": "trip",
            "title": "旅行プランの確認",
            "intro": "ぴったりの旅程を提案するために、いくつか教えてください。",
            "submit": { "action": "answer" },
            "submit_label": "回答する",
            "questions": [
                {
                    "id": "purpose",
                    "header": "目的",
                    "question": "今回の旅行の主な目的は何ですか？",
                    "options": [
                        { "label": "観光・レジャー", "description": "名所や自然、グルメなど旅先を楽しむのが中心" },
                        { "label": "出張・ビジネス", "description": "会議や商談が主目的。移動効率と宿の作業環境を重視" },
                        { "label": "帰省・イベント", "description": "家族の集まりや結婚式・ライブなど特定の予定に合わせる" }
                    ],
                    "allow_other": true
                },
                {
                    "id": "pace",
                    "header": "ペース",
                    "question": "旅のペースはどれくらいが好みですか？",
                    "options": [
                        { "label": "ゆったり", "description": "1 日 1〜2 か所。休憩やカフェの時間をしっかり取る" },
                        { "label": "しっかり", "description": "主要スポットを効率よく巡る、バランス型" },
                        { "label": "詰め込み", "description": "朝から晩まで、行けるところは全部回りたい" }
                    ]
                },
                {
                    "id": "interests",
                    "header": "興味",
                    "question": "特に興味があるものはどれですか？（複数選択できます）",
                    "options": [
                        { "label": "グルメ", "description": "地元の名物や話題の店を巡りたい" },
                        { "label": "自然・絶景", "description": "山・海・公園など景色を楽しみたい" },
                        { "label": "歴史・文化", "description": "寺社・城・博物館など" },
                        { "label": "ショッピング", "description": "買い物や土産選びを楽しみたい" }
                    ],
                    "multi_select": true,
                    "allow_other": true
                },
                {
                    "id": "notes",
                    "question": "その他、希望や制約があれば自由にお書きください。",
                    "placeholder": "例: 子ども連れ／車椅子で移動／予算は 1 人 5 万円まで など"
                }
            ]
        }
    })
}

/// 地図（マーカー＋ルート）の固定スペック。座標のみで完結し、タイルはサーバ設定で注入される。
/// 東京の半日さんぽ（駅→タワー→美術館→庭園）を徒歩ルートで示す。
fn map_spec() -> Value {
    // 実際の街路に沿わせた密なポリライン（AI が経路ツールで出す想定・クライアントは外部照会しない）。
    // json! の再帰展開上限に当たるため配列は tuple slice から先に組む。
    const ROUTE: &[(f64, f64)] = &[
        (35.6812, 139.76711),
        (35.68048, 139.7659),
        (35.67941, 139.7639),
        (35.67862, 139.76293),
        (35.67833, 139.76216),
        (35.67545, 139.75974),
        (35.67409, 139.75835),
        (35.6697, 139.75528),
        (35.66782, 139.75447),
        (35.6647, 139.75302),
        (35.66301, 139.75222),
        (35.66104, 139.75125),
        (35.6599, 139.74957),
        (35.65971, 139.74723),
        (35.65917, 139.74533),
        (35.65952, 139.74313),
        (35.66007, 139.74055),
        (35.66139, 139.73704),
        (35.662, 139.73496),
        (35.66228, 139.73271),
        (35.66113, 139.73045),
        (35.66068, 139.72955),
        (35.66075, 139.72969),
        (35.6617, 139.73023),
        (35.6619, 139.73046),
        (35.66337, 139.73199),
        (35.66431, 139.73404),
        (35.6685, 139.74013),
        (35.67054, 139.74219),
        (35.6719, 139.74379),
        (35.67372, 139.74743),
        (35.67667, 139.74926),
        (35.67737, 139.7505),
        (35.677, 139.75579),
        (35.68452, 139.76027),
        (35.68588, 139.76037),
        (35.68607, 139.75804),
        (35.68518, 139.75687),
        (35.68573, 139.75517),
        (35.68616, 139.75473),
        (35.68574, 139.75536),
        (35.68501, 139.75729),
        (35.68623, 139.75827),
        (35.68572, 139.761),
        (35.68466, 139.76278),
        (35.68221, 139.76346),
        (35.67942, 139.76275),
        (35.67587, 139.76286),
        (35.67488, 139.76303),
    ];
    let waypoints: Vec<Value> = ROUTE
        .iter()
        .map(|(lat, lng)| json!({ "lat": lat, "lng": lng }))
        .collect();
    json!({
        "version": 1,
        "root": {
            "component": "map",
            "title": "東京 半日さんぽ（徒歩ルート）",
            "center": { "lat": 35.665, "lng": 139.752 },
            "zoom": 13,
            "markers": [
                { "lat": 35.6812, "lng": 139.7671, "label": "東京駅", "description": "出発地・10:00", "kind": "start" },
                { "lat": 35.6586, "lng": 139.7454, "label": "東京タワー", "description": "展望・11:00", "kind": "sight" },
                { "lat": 35.6604, "lng": 139.7292, "label": "六本木で昼食", "description": "12:30", "kind": "food" },
                { "lat": 35.6852, "lng": 139.7528, "label": "皇居東御苑", "description": "散策・14:30", "kind": "sight" },
                { "lat": 35.6749, "lng": 139.763, "label": "有楽町のホテル", "description": "チェックイン・16:00", "kind": "lodging" }
            ],
            "route": { "mode": "walking", "waypoints": waypoints }
        }
    })
}

/// ドメインカード（RAG 引用元・旅程・天気・比較・タイムライン）を 1 コンテナにまとめた固定スペック。
fn domain_spec() -> Value {
    json!({
        "version": 1,
        "root": {
            "component": "container",
            "children": [
                {
                    "component": "source_card",
                    "title": "参照した資料",
                    "sources": [
                        { "title": "設計ドキュメント", "snippet": "二段 authz は pre/post filter…", "url": "https://example.com/d", "score": 0.94, "label": "PDF" },
                        { "title": "オンボーディング", "snippet": "AuthContext 経由で全アクセス", "url": "https://example.com/g", "score": 0.81, "label": "Web" }
                    ]
                },
                {
                    "component": "itinerary",
                    "title": "東京 日帰りプラン",
                    "days": [
                        { "label": "1 日目", "date": "7/13(日)", "items": [
                            { "time": "10:00", "title": "東京駅 集合", "location": "丸の内北口", "kind": "travel" },
                            { "time": "12:30", "title": "六本木でランチ", "kind": "food" }
                        ]}
                    ]
                },
                {
                    "component": "weather",
                    "location": "東京の天気",
                    "days": [
                        { "label": "今日", "condition": "sunny", "high": 31.0, "low": 24.0, "precipitation": 10.0 },
                        { "label": "明日", "condition": "rain", "high": 26.0, "low": 22.0, "precipitation": 80.0 }
                    ]
                },
                {
                    "component": "comparison",
                    "title": "プラン比較",
                    "columns": ["Free", "Pro", "Enterprise"],
                    "highlight": 1,
                    "rows": [
                        { "label": "月額", "values": ["¥0", "¥1,480", "要問合せ"] },
                        { "label": "容量", "values": ["1GB", "100GB", "無制限"] }
                    ]
                },
                {
                    "component": "timeline",
                    "title": "リリース履歴",
                    "events": [
                        { "time": "2026-06", "title": "generative UI", "tone": "info" },
                        { "time": "2026-07", "title": "genui 拡充", "tone": "warning" }
                    ]
                }
            ]
        }
    })
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

    #[test]
    fn genui_question_spec_shape() {
        // question カードは chat.submit 束縛＋複数質問を持つ（検証を通る形）。
        let spec = genui_spec("question");
        assert_eq!(spec["root"]["component"], "question_card");
        assert_eq!(spec["actions"][0]["handler"], "chat.submit");
        assert!(spec["root"]["questions"].as_array().unwrap().len() >= 3);
    }

    #[test]
    fn genui_map_spec_shape() {
        // 地図はマーカー＋ルート waypoint を持つ（座標のみ・タイル URL 無し）。
        let spec = genui_spec("map");
        assert_eq!(spec["root"]["component"], "map");
        assert!(spec["root"]["markers"].as_array().unwrap().len() >= 2);
        assert_eq!(spec["root"]["route"]["mode"], "walking");
        assert!(spec["root"]["route"]["waypoints"].as_array().unwrap().len() >= 2);
    }

    #[test]
    fn genui_domain_spec_shape() {
        // ドメインカードは 5 種を 1 コンテナに束ねる（すべて検証を通る形）。
        let spec = genui_spec("domain");
        assert_eq!(spec["root"]["component"], "container");
        let kinds: Vec<&str> = spec["root"]["children"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["component"].as_str().unwrap())
            .collect();
        assert_eq!(
            kinds,
            [
                "source_card",
                "itinerary",
                "weather",
                "comparison",
                "timeline"
            ]
        );
    }
}
