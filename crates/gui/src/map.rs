//! 地図コンポーネント（MapLibre GL・PR5）。
//!
//! AI は緯度経度・マーカー・ルート waypoint など**構造化データのみ**を emit する。
//! タイル/スタイルの URL は AI ではなく**サーバ設定**で注入し（信頼境界を維持）、未設定時は
//! フロントが自己完結のオフライン既定スタイルで描画する（air-gapped/CI でも決定論的）。
//! 任意 URL/コード/HTML はこの型で表現不可能（閉じた集合・`deny_unknown_fields`）。座標範囲・
//! 件数の検証は [`validate`](crate::validate) がツリー走査で掛ける。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// 地図（マーカー＋任意のルート）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MapProps {
    /// 初期表示の中心。
    pub center: GeoPoint,
    /// 初期ズーム（0=世界全体〜22=建物）。省略時はフロントが markers/bounds から決める。
    #[serde(default)]
    pub zoom: Option<f64>,
    /// マーカー（地点ピン）。
    #[serde(default)]
    pub markers: Vec<MapMarker>,
    /// ルート（順序付き waypoint を結ぶ線・任意）。
    #[serde(default)]
    pub route: Option<MapRoute>,
    /// 表示範囲（指定時は markers/center より優先して収める・任意）。
    #[serde(default)]
    pub bounds: Option<GeoBounds>,
    /// 地図の見出し（任意）。
    #[serde(default)]
    pub title: Option<String>,
}

/// 緯度経度（WGS84・度）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct GeoPoint {
    /// 緯度（-90〜90）。
    pub lat: f64,
    /// 経度（-180〜180）。
    pub lng: f64,
}

/// 地点マーカー。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MapMarker {
    /// 緯度（-90〜90）。
    pub lat: f64,
    /// 経度（-180〜180）。
    pub lng: f64,
    /// ピンのラベル（地名など・任意）。
    #[serde(default)]
    pub label: Option<String>,
    /// 補足の説明（任意）。
    #[serde(default)]
    pub description: Option<String>,
    /// マーカー種別（配色/アイコンの意味付け）。
    #[serde(default)]
    pub kind: MarkerKind,
}

/// マーカー種別（意味的バリアントのみ・配色は閉じた集合から選ぶ）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum MarkerKind {
    /// 一般的な地点。
    #[default]
    Place,
    /// 出発地。
    Start,
    /// 到着地。
    End,
    /// 経由地。
    Stop,
    /// 宿泊。
    Lodging,
    /// 飲食。
    Food,
    /// 観光・見どころ。
    Sight,
}

/// ルート（順序付き地点を結ぶ線）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MapRoute {
    /// 経由地（順序どおりに結ぶ・2 点以上）。
    pub waypoints: Vec<GeoPoint>,
    /// 移動手段（配色/表現の意味付け）。
    #[serde(default)]
    pub mode: RouteMode,
}

/// 移動手段（意味的バリアントのみ）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum RouteMode {
    /// 自動車。
    #[default]
    Driving,
    /// 徒歩。
    Walking,
    /// 公共交通。
    Transit,
    /// 航空。
    Flight,
}

/// 表示範囲（南西 / 北東の角）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct GeoBounds {
    /// 南端の緯度。
    pub south: f64,
    /// 西端の経度。
    pub west: f64,
    /// 北端の緯度。
    pub north: f64,
    /// 東端の経度。
    pub east: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialize/Deserialize の両方向を通す（型のみモジュールのカバレッジ確保）。
    #[test]
    fn map_props_roundtrip() {
        let props = MapProps {
            center: GeoPoint {
                lat: 35.681,
                lng: 139.767,
            },
            zoom: Some(11.0),
            markers: vec![
                MapMarker {
                    lat: 35.681,
                    lng: 139.767,
                    label: Some("東京駅".into()),
                    description: Some("出発地".into()),
                    kind: MarkerKind::Start,
                },
                MapMarker {
                    lat: 35.658,
                    lng: 139.745,
                    label: Some("東京タワー".into()),
                    description: None,
                    kind: MarkerKind::Sight,
                },
            ],
            route: Some(MapRoute {
                waypoints: vec![
                    GeoPoint {
                        lat: 35.681,
                        lng: 139.767,
                    },
                    GeoPoint {
                        lat: 35.658,
                        lng: 139.745,
                    },
                ],
                mode: RouteMode::Walking,
            }),
            bounds: Some(GeoBounds {
                south: 35.65,
                west: 139.74,
                north: 35.69,
                east: 139.77,
            }),
            title: Some("東京 半日さんぽ".into()),
        };
        let json = serde_json::to_value(&props).unwrap();
        assert_eq!(json["markers"][0]["kind"], "start");
        assert_eq!(json["route"]["mode"], "walking");
        let back: MapProps = serde_json::from_value(json).unwrap();
        assert_eq!(back, props);
    }

    /// 既定値（kind/mode）の serde 名を固定する。
    #[test]
    fn defaults_serialize_stable() {
        assert_eq!(
            serde_json::to_value(MarkerKind::default()).unwrap(),
            serde_json::json!("place")
        );
        assert_eq!(
            serde_json::to_value(RouteMode::default()).unwrap(),
            serde_json::json!("driving")
        );
    }
}
