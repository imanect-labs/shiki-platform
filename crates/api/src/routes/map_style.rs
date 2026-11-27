//! 背景地図のスタイル（MapLibre style spec v8・OpenMapTiles のレイヤ構成）。
//!
//! 図の地点・道順を読みやすくするための控えめな地図にする（道路・水域・緑地・建物・地名だけ）。
//! 地図アプリでなじみのある配色（灰色の地・白い道路・黄色の幹線・青い水域・緑の公園）にし、
//! 場所が読み取れる濃さを保ちつつ、地点の赤い点・札とは紛れないようにする。
//! `glyphs` と `sprite` は持たない。文字はすべて端末のフォントで描き（MapLibre は `glyphs` の無い
//! スタイルの文字を手元で描く）、アイコンは使わない。ブラウザは地図データ以外を読みに行かない。

use serde::Deserialize;
use serde_json::{json, Value};
use utoipa::ToSchema;

/// 地図データ（OpenMapTiles 形式・OSM 由来）の出典。利用条件として地図に必ず表示する。
pub const ATTRIBUTION: [(&str, &str); 2] = [
    (
        "© OpenStreetMap contributors",
        "https://www.openstreetmap.org/copyright",
    ),
    ("© OpenMapTiles", "https://openmaptiles.org/"),
];

/// スタイルの中のソース名。
pub const SOURCE: &str = "openmaptiles";

/// 配色。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MapTheme {
    #[default]
    Light,
    Dark,
}

/// 配色の値。
struct Palette {
    background: &'static str,
    water: &'static str,
    waterway: &'static str,
    wood: &'static str,
    grass: &'static str,
    residential: &'static str,
    commercial: &'static str,
    building: &'static str,
    building_outline: &'static str,
    motorway: &'static str,
    motorway_casing: &'static str,
    major: &'static str,
    major_casing: &'static str,
    minor: &'static str,
    minor_casing: &'static str,
    path: &'static str,
    rail: &'static str,
    boundary: &'static str,
    text: &'static str,
    text_muted: &'static str,
    water_text: &'static str,
    halo: &'static str,
}

const LIGHT: Palette = Palette {
    background: "#f3f4f6",
    water: "#a9d3f2",
    waterway: "#9ecbec",
    wood: "#cfe7c9",
    grass: "#dbeed5",
    residential: "#eef0f2",
    commercial: "#f1edf0",
    building: "#e4e6ea",
    building_outline: "#d5d8de",
    motorway: "#f8c967",
    motorway_casing: "#e2a93c",
    major: "#fde49a",
    major_casing: "#e9cd78",
    minor: "#ffffff",
    minor_casing: "#d6d9de",
    path: "#c4c8cf",
    rail: "#a8adb5",
    boundary: "#a1a6b8",
    text: "#2f3337",
    text_muted: "#5f6368",
    water_text: "#3b77a8",
    halo: "#ffffff",
};

const DARK: Palette = Palette {
    background: "#1b1d21",
    water: "#17304a",
    waterway: "#1c3a58",
    wood: "#1d2a22",
    grass: "#202c24",
    residential: "#212328",
    commercial: "#24232a",
    building: "#2a2c32",
    building_outline: "#33363d",
    motorway: "#8a6b33",
    motorway_casing: "#5e4a25",
    major: "#5c5438",
    major_casing: "#3d3826",
    minor: "#3a3d45",
    minor_casing: "#2a2c32",
    path: "#454850",
    rail: "#565a63",
    boundary: "#6e6a86",
    text: "#e8e6e3",
    text_muted: "#b4b1ad",
    water_text: "#8cb8de",
    halo: "#1b1d21",
};

/// 地名の文字（日本語名を優先し、無ければ既定の名前）。
fn name_expr() -> Value {
    json!(["coalesce", ["get", "name:ja"], ["get", "name"]])
}

/// ズームに応じて線幅を変える式（`stops` は `[zoom, 幅]`）。
fn width(stops: &[(f64, f64)]) -> Value {
    let mut v = vec![
        json!("interpolate"),
        json!(["exponential", 1.5]),
        json!(["zoom"]),
    ];
    for (z, w) in stops {
        v.push(json!(z));
        v.push(json!(w));
    }
    Value::Array(v)
}

/// 道路の 1 区分（縁取りと中身の 2 レイヤ）。
fn road_layers(
    id: &str,
    classes: &[&str],
    min_zoom: f64,
    fill: &str,
    casing: &str,
    widths: &[(f64, f64)],
) -> [Value; 2] {
    let filter = json!([
        "all",
        ["match", ["get", "class"], classes, true, false],
        ["!=", ["get", "brunnel"], "tunnel"]
    ]);
    let casing_widths: Vec<(f64, f64)> = widths.iter().map(|(z, w)| (*z, w + 1.5)).collect();
    [
        json!({
            "id": format!("road-{id}-casing"), "type": "line", "source": SOURCE,
            "source-layer": "transportation", "minzoom": min_zoom, "filter": filter,
            "layout": { "line-cap": "round", "line-join": "round" },
            "paint": { "line-color": casing, "line-width": width(&casing_widths) }
        }),
        json!({
            "id": format!("road-{id}"), "type": "line", "source": SOURCE,
            "source-layer": "transportation", "minzoom": min_zoom, "filter": filter,
            "layout": { "line-cap": "round", "line-join": "round" },
            "paint": { "line-color": fill, "line-width": width(widths) }
        }),
    ]
}

/// 地色・土地利用・水域・建物・小道（道路より下に描くもの）。
fn ground_layers(p: &Palette) -> Vec<Value> {
    vec![
        json!({ "id": "background", "type": "background", "paint": { "background-color": p.background } }),
        json!({
            "id": "landcover-wood", "type": "fill", "source": SOURCE, "source-layer": "landcover",
            "filter": ["match", ["get", "class"], ["wood", "forest"], true, false],
            "paint": { "fill-color": p.wood }
        }),
        json!({
            "id": "landcover-grass", "type": "fill", "source": SOURCE, "source-layer": "landcover",
            "filter": ["match", ["get", "class"], ["grass", "farmland", "wetland"], true, false],
            "paint": { "fill-color": p.grass, "fill-opacity": 0.8 }
        }),
        json!({
            "id": "landuse-residential", "type": "fill", "source": SOURCE, "source-layer": "landuse",
            "minzoom": 9,
            "filter": ["match", ["get", "class"], ["residential", "suburb", "neighbourhood"], true, false],
            "paint": { "fill-color": p.residential }
        }),
        json!({
            "id": "landuse-commercial", "type": "fill", "source": SOURCE, "source-layer": "landuse",
            "minzoom": 11,
            "filter": ["match", ["get", "class"], ["commercial", "retail", "industrial", "railway"], true, false],
            "paint": { "fill-color": p.commercial }
        }),
        json!({
            "id": "park", "type": "fill", "source": SOURCE, "source-layer": "park",
            "paint": { "fill-color": p.grass }
        }),
        json!({
            "id": "water", "type": "fill", "source": SOURCE, "source-layer": "water",
            "paint": { "fill-color": p.water }
        }),
        json!({
            "id": "waterway", "type": "line", "source": SOURCE, "source-layer": "waterway",
            "minzoom": 8,
            "paint": { "line-color": p.waterway, "line-width": width(&[(8.0, 0.5), (14.0, 2.0), (18.0, 6.0)]) }
        }),
        json!({
            "id": "building", "type": "fill", "source": SOURCE, "source-layer": "building",
            "minzoom": 14,
            "paint": { "fill-color": p.building, "fill-outline-color": p.building_outline }
        }),
        json!({
            "id": "road-path", "type": "line", "source": SOURCE, "source-layer": "transportation",
            "minzoom": 14,
            "filter": ["match", ["get", "class"], ["path", "track"], true, false],
            "paint": { "line-color": p.path, "line-width": 1, "line-dasharray": [2, 1.5] }
        }),
    ]
}

/// 道路（細い順に下から重ねる）。
fn all_road_layers(p: &Palette) -> Vec<Value> {
    let mut layers = Vec::new();
    layers.extend(road_layers(
        "minor",
        &["minor", "service"],
        12.0,
        p.minor,
        p.minor_casing,
        &[(12.0, 0.5), (14.0, 2.5), (18.0, 12.0)],
    ));
    layers.extend(road_layers(
        "major",
        &["primary", "secondary", "tertiary", "trunk"],
        7.0,
        p.major,
        p.major_casing,
        &[(7.0, 0.6), (12.0, 2.0), (14.0, 4.0), (18.0, 18.0)],
    ));
    layers.extend(road_layers(
        "motorway",
        &["motorway"],
        5.0,
        p.motorway,
        p.motorway_casing,
        &[(5.0, 0.6), (12.0, 2.5), (14.0, 4.5), (18.0, 20.0)],
    ));
    layers
}

/// 鉄道・境界・地名（道路より上に描くもの）。
fn overlay_layers(p: &Palette) -> Vec<Value> {
    vec![
        json!({
            "id": "rail", "type": "line", "source": SOURCE, "source-layer": "transportation",
            "minzoom": 10,
            "filter": ["match", ["get", "class"], ["rail", "transit"], true, false],
            "paint": { "line-color": p.rail, "line-width": width(&[(10.0, 0.6), (16.0, 2.0)]) }
        }),
        json!({
            "id": "boundary-municipal", "type": "line", "source": SOURCE, "source-layer": "boundary",
            "minzoom": 9,
            "filter": ["all", [">=", ["get", "admin_level"], 5], ["<=", ["get", "admin_level"], 8], ["!=", ["get", "maritime"], 1]],
            "paint": { "line-color": p.boundary, "line-width": 0.8, "line-dasharray": [3, 2], "line-opacity": 0.6 }
        }),
        json!({
            "id": "boundary-region", "type": "line", "source": SOURCE, "source-layer": "boundary",
            "filter": ["all", ["<=", ["get", "admin_level"], 4], ["!=", ["get", "maritime"], 1]],
            "paint": { "line-color": p.boundary, "line-width": width(&[(3.0, 0.8), (12.0, 2.0)]), "line-dasharray": [4, 2] }
        }),
        json!({
            "id": "water-name", "type": "symbol", "source": SOURCE, "source-layer": "water_name",
            "layout": { "text-field": name_expr(), "text-size": 12, "text-max-width": 8 },
            "paint": { "text-color": p.water_text, "text-halo-color": p.halo, "text-halo-width": 1.2 }
        }),
        json!({
            "id": "road-name", "type": "symbol", "source": SOURCE, "source-layer": "transportation_name",
            "minzoom": 12,
            // 鉄道の線名は出さない（駅のまわりで文字が重なり、地点の名前が読みにくくなる）。
            "filter": ["!", ["match", ["get", "class"], ["rail", "transit"], true, false]],
            "layout": {
                "symbol-placement": "line", "text-field": name_expr(), "text-size": 11.5,
                "text-rotation-alignment": "map", "text-pitch-alignment": "viewport"
            },
            "paint": { "text-color": p.text_muted, "text-halo-color": p.halo, "text-halo-width": 1.5 }
        }),
        json!({
            "id": "place-minor", "type": "symbol", "source": SOURCE, "source-layer": "place",
            "minzoom": 10,
            "filter": ["match", ["get", "class"], ["suburb", "quarter", "neighbourhood", "village", "hamlet"], true, false],
            "layout": { "text-field": name_expr(), "text-size": 12.5, "text-max-width": 8 },
            "paint": { "text-color": p.text_muted, "text-halo-color": p.halo, "text-halo-width": 1.8 }
        }),
        // 駅名（どのあたりかの目印）。
        json!({
            "id": "station-name", "type": "symbol", "source": SOURCE, "source-layer": "poi",
            "minzoom": 11,
            "filter": ["all", ["==", ["get", "class"], "railway"], ["match", ["get", "subclass"], ["station", "halt"], true, false]],
            "layout": { "text-field": name_expr(), "text-size": 12, "text-max-width": 8 },
            "paint": { "text-color": p.text, "text-halo-color": p.halo, "text-halo-width": 1.8 }
        }),
        json!({
            "id": "place-major", "type": "symbol", "source": SOURCE, "source-layer": "place",
            "filter": ["match", ["get", "class"], ["city", "town"], true, false],
            "layout": {
                "text-field": name_expr(),
                "text-size": ["interpolate", ["linear"], ["zoom"], 6, 12, 12, 16],
                "text-max-width": 8
            },
            "paint": { "text-color": p.text, "text-halo-color": p.halo, "text-halo-width": 1.8 }
        }),
    ]
}

/// スタイル JSON を組む。`archive_path` は地図データの場所（API の根からの相対パス）。
pub fn basemap_style(theme: MapTheme, archive_path: &str) -> Value {
    let p = match theme {
        MapTheme::Light => &LIGHT,
        MapTheme::Dark => &DARK,
    };
    let attribution = ATTRIBUTION
        .iter()
        .map(|(text, url)| {
            format!("<a href=\"{url}\" target=\"_blank\" rel=\"noopener\">{text}</a>")
        })
        .collect::<Vec<_>>()
        .join(" ");
    let mut layers = ground_layers(p);
    layers.extend(all_road_layers(p));
    layers.extend(overlay_layers(p));
    json!({
        "version": 8,
        "name": "shiki-basemap",
        "sources": {
            SOURCE: {
                "type": "vector",
                "url": format!("pmtiles://{archive_path}"),
                "attribution": attribution
            }
        },
        "layers": layers
    })
}
