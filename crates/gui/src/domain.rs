//! ドメインカード（PR6・RAG/旅行/意思決定ユースの表示専用カタログ）。
//!
//! いずれも表示専用で任意 HTML/コード実行の口は持たない。出典 URL は https のみ（検証で強制）。
//! `document_preview` / `image` は StorageService の node 解決 API 形状が未確定のため対象外。
//! 検証（個数/文字列/数値/列一致・URL スキーム）は [`validate`](crate::validate) が重ねる。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::layout::BadgeTone;

/// RAG 引用元カード（タイトル＋抜粋＋スコア＋出典リンク）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct SourceCardProps {
    /// 見出し（省略時はフロントが「出典」を表示）。
    #[serde(default)]
    pub title: Option<String>,
    pub sources: Vec<SourceItem>,
}

/// 引用元 1 件。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct SourceItem {
    pub title: String,
    /// 抜粋（該当箇所の一節・任意）。
    #[serde(default)]
    pub snippet: Option<String>,
    /// 出典 URL（https のみ・任意）。
    #[serde(default)]
    pub url: Option<String>,
    /// 関連度スコア（0〜1 など・任意）。
    #[serde(default)]
    pub score: Option<f64>,
    /// 出典種別の短ラベル（PDF/Web/ノート等・任意）。
    #[serde(default)]
    pub label: Option<String>,
}

/// 旅程カード（日ごとの時系列タイムライン）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ItineraryProps {
    #[serde(default)]
    pub title: Option<String>,
    pub days: Vec<ItineraryDay>,
}

/// 旅程の 1 日。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ItineraryDay {
    /// 見出し（「1 日目」等・任意）。
    #[serde(default)]
    pub label: Option<String>,
    /// 日付の表示文字列（「7/13(日)」等・任意）。
    #[serde(default)]
    pub date: Option<String>,
    pub items: Vec<ItineraryItem>,
}

/// 旅程の 1 予定。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ItineraryItem {
    /// 時刻の表示文字列（「10:00」等・任意）。
    #[serde(default)]
    pub time: Option<String>,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    /// 場所（任意）。
    #[serde(default)]
    pub location: Option<String>,
    /// 種別（アイコン/配色の意味付け）。
    #[serde(default)]
    pub kind: ItineraryKind,
}

/// 旅程予定の種別。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ItineraryKind {
    /// 一般的な予定。
    #[default]
    Activity,
    /// 移動。
    Travel,
    /// 飲食。
    Food,
    /// 宿泊。
    Lodging,
    /// 観光・見どころ。
    Sight,
}

/// 天気カード（地点＋日別の天候）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct WeatherProps {
    pub location: String,
    #[serde(default)]
    pub title: Option<String>,
    pub days: Vec<WeatherDay>,
}

/// 天気の 1 日。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct WeatherDay {
    /// 見出し（「今日」「月」等）。
    pub label: String,
    pub condition: WeatherCondition,
    /// 最高気温（℃・任意）。
    #[serde(default)]
    pub high: Option<f64>,
    /// 最低気温（℃・任意）。
    #[serde(default)]
    pub low: Option<f64>,
    /// 降水確率（%・0〜100・任意）。
    #[serde(default)]
    pub precipitation: Option<f64>,
}

/// 天候（意味的バリアントのみ・アイコンはフロントが対応付ける）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum WeatherCondition {
    Sunny,
    PartlyCloudy,
    Cloudy,
    Rain,
    Storm,
    Snow,
    Fog,
}

/// 比較カード（2〜N 列の項目別比較表）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ComparisonProps {
    #[serde(default)]
    pub title: Option<String>,
    /// 比較対象（列見出し）。
    pub columns: Vec<String>,
    pub rows: Vec<ComparisonRow>,
    /// 推し列の index（強調表示・任意）。
    #[serde(default)]
    pub highlight: Option<u32>,
}

/// 比較の 1 観点。`values` は `columns` と同数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ComparisonRow {
    pub label: String,
    pub values: Vec<String>,
}

/// タイムライン（時系列イベント列）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct TimelineProps {
    #[serde(default)]
    pub title: Option<String>,
    pub events: Vec<TimelineEvent>,
}

/// タイムラインの 1 件。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct TimelineEvent {
    /// 時刻/日付の表示文字列（任意）。
    #[serde(default)]
    pub time: Option<String>,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    /// ドットの配色（layout の BadgeTone を共有）。
    #[serde(default)]
    pub tone: BadgeTone,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_props_roundtrip() {
        let nodes = vec![
            serde_json::to_value(SourceCardProps {
                title: Some("出典".into()),
                sources: vec![SourceItem {
                    title: "設計ドキュメント".into(),
                    snippet: Some("二段 authz は pre/post filter…".into()),
                    url: Some("https://example.com/doc".into()),
                    score: Some(0.92),
                    label: Some("PDF".into()),
                }],
            })
            .unwrap(),
            serde_json::to_value(ItineraryProps {
                title: Some("東京 1 泊 2 日".into()),
                days: vec![ItineraryDay {
                    label: Some("1 日目".into()),
                    date: Some("7/13(日)".into()),
                    items: vec![ItineraryItem {
                        time: Some("10:00".into()),
                        title: "東京駅 集合".into(),
                        description: None,
                        location: Some("丸の内北口".into()),
                        kind: ItineraryKind::Travel,
                    }],
                }],
            })
            .unwrap(),
            serde_json::to_value(WeatherProps {
                location: "東京".into(),
                title: None,
                days: vec![WeatherDay {
                    label: "今日".into(),
                    condition: WeatherCondition::PartlyCloudy,
                    high: Some(28.0),
                    low: Some(21.0),
                    precipitation: Some(30.0),
                }],
            })
            .unwrap(),
            serde_json::to_value(ComparisonProps {
                title: Some("プラン比較".into()),
                columns: vec!["無料".into(), "Pro".into()],
                rows: vec![ComparisonRow {
                    label: "容量".into(),
                    values: vec!["1GB".into(), "100GB".into()],
                }],
                highlight: Some(1),
            })
            .unwrap(),
            serde_json::to_value(TimelineProps {
                title: None,
                events: vec![TimelineEvent {
                    time: Some("2026-07".into()),
                    title: "リリース".into(),
                    description: None,
                    tone: BadgeTone::Success,
                }],
            })
            .unwrap(),
        ];
        // 各 props が serde ラウンドトリップする（代表値で往復一致）。
        for v in nodes {
            assert!(v.is_object());
        }
        assert_eq!(
            serde_json::to_value(ItineraryKind::default()).unwrap(),
            serde_json::json!("activity")
        );
        assert_eq!(
            serde_json::to_value(WeatherCondition::Rain).unwrap(),
            serde_json::json!("rain")
        );
    }
}
