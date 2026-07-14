//! 地図（PR5）の検証（[`Walk`](super::Walk) の一部・ファイル分割）。
//!
//! URL/タイルは AI が持たない（サーバ設定）ため検証対象は座標範囲・件数・ズーム・ラベル長など
//! 構造化データのみ。

use super::Walk;
use crate::validate::{limits, GuiValidationError};

impl Walk<'_> {
    /// 地図。座標範囲・件数・ズーム・ラベル長を検証する。
    pub(super) fn map(&mut self, p: &crate::map::MapProps, path: &str) {
        self.opt_label(p.title.as_deref(), &format!("{path}.title"));
        self.geo_point(&p.center, &format!("{path}.center"));
        if let Some(z) = p.zoom {
            if !(z.is_finite() && (0.0..=limits::MAX_MAP_ZOOM).contains(&z)) {
                self.errors.push(
                    GuiValidationError::new(
                        "gui.invalid_range",
                        format!("zoom は 0〜{} の有限数のみ", limits::MAX_MAP_ZOOM),
                    )
                    .at(format!("{path}.zoom")),
                );
            }
        }
        if p.markers.len() > limits::MAX_MAP_MARKERS {
            self.errors.push(
                GuiValidationError::new(
                    "gui.too_many_markers",
                    format!("マーカーが多すぎます（最大 {}）", limits::MAX_MAP_MARKERS),
                )
                .at(path),
            );
        }
        for (i, m) in p.markers.iter().enumerate() {
            let mpath = format!("{path}.markers[{i}]");
            self.geo_coord(m.lat, m.lng, &mpath);
            self.opt_label(m.label.as_deref(), &format!("{mpath}.label"));
            if let Some(d) = &m.description {
                self.text(d, limits::MAX_TEXT_CHARS, &format!("{mpath}.description"));
            }
        }
        if let Some(route) = &p.route {
            let rpath = format!("{path}.route");
            // 2 点未満のルートは線を描けない（点は marker で表す）。
            if route.waypoints.len() < 2 {
                self.errors.push(
                    GuiValidationError::new(
                        "gui.invalid_route",
                        "route.waypoints は 2 点以上必要です",
                    )
                    .at(&rpath),
                );
            }
            if route.waypoints.len() > limits::MAX_ROUTE_WAYPOINTS {
                self.errors.push(
                    GuiValidationError::new(
                        "gui.too_many_waypoints",
                        format!(
                            "waypoint が多すぎます（最大 {}）",
                            limits::MAX_ROUTE_WAYPOINTS
                        ),
                    )
                    .at(&rpath),
                );
            }
            for (i, wp) in route.waypoints.iter().enumerate() {
                self.geo_point(wp, &format!("{rpath}.waypoints[{i}]"));
            }
        }
        if let Some(b) = &p.bounds {
            let bpath = format!("{path}.bounds");
            self.geo_coord(b.south, b.west, &bpath);
            self.geo_coord(b.north, b.east, &bpath);
            // 南 ≤ 北（経度は日付変更線跨ぎがあり得るので west/east は順序を課さない）。
            if b.south.is_finite() && b.north.is_finite() && b.south > b.north {
                self.errors.push(
                    GuiValidationError::new(
                        "gui.invalid_bounds",
                        "bounds は south ≤ north が必要です",
                    )
                    .at(&bpath),
                );
            }
        }
    }

    /// 緯度経度点の範囲検証。
    fn geo_point(&mut self, pt: &crate::map::GeoPoint, path: &str) {
        self.geo_coord(pt.lat, pt.lng, path);
    }

    /// 緯度 lat∈[-90,90] / 経度 lng∈[-180,180]・ともに有限数を課す。
    fn geo_coord(&mut self, lat: f64, lng: f64, path: &str) {
        if !(lat.is_finite() && (-90.0..=90.0).contains(&lat)) {
            self.errors.push(
                GuiValidationError::new("gui.invalid_coord", "lat は -90〜90 の有限数のみ")
                    .at(format!("{path}.lat")),
            );
        }
        if !(lng.is_finite() && (-180.0..=180.0).contains(&lng)) {
            self.errors.push(
                GuiValidationError::new("gui.invalid_coord", "lng は -180〜180 の有限数のみ")
                    .at(format!("{path}.lng")),
            );
        }
    }
}
