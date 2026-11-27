//! `routes::map_route` のユニットテスト（polyline の復号・応答の解釈・入力の検証）。

use super::*;

/// テスト用の符号化（精度 6 桁・`[経度, 緯度]` の列）。
pub(crate) fn encode_polyline6(coords: &[[f64; 2]]) -> String {
    fn push(out: &mut String, v: i64) {
        let mut v = if v < 0 { !(v << 1) } else { v << 1 };
        while v >= 0x20 {
            out.push(char::from(u8::try_from((0x20 | (v & 0x1f)) + 63).unwrap()));
            v >>= 5;
        }
        out.push(char::from(u8::try_from(v + 63).unwrap()));
    }
    let (mut plat, mut plng) = (0i64, 0i64);
    let mut out = String::new();
    for [lng, lat] in coords {
        let (lat, lng) = ((lat * 1e6).round() as i64, (lng * 1e6).round() as i64);
        push(&mut out, lat - plat);
        push(&mut out, lng - plng);
        (plat, plng) = (lat, lng);
    }
    out
}

#[test]
fn decodes_precision6_polyline() {
    // 精度 5 桁の定番の例を精度 6 桁として読むと、値は 10 分の 1 になる。
    let got = decode_polyline6("_p~iF~ps|U_ulLnnqC_mqNvxq`@").unwrap();
    let want = [[-12.02, 3.85], [-12.095, 4.07], [-12.6453, 4.3252]];
    assert_eq!(got.len(), want.len());
    for (g, w) in got.iter().zip(want) {
        assert!(
            (g[0] - w[0]).abs() < 1e-9 && (g[1] - w[1]).abs() < 1e-9,
            "{g:?} {w:?}"
        );
    }
    // 往復できる。
    let coords = [[139.7671, 35.6812], [139.764, 35.6795], [139.77, 35.683]];
    let back = decode_polyline6(&encode_polyline6(&coords)).unwrap();
    for (g, w) in back.iter().zip(coords) {
        assert!((g[0] - w[0]).abs() < 1e-9 && (g[1] - w[1]).abs() < 1e-9);
    }
    assert_eq!(decode_polyline6("").unwrap(), Vec::<[f64; 2]>::new());
}

#[test]
fn rejects_broken_polyline() {
    // 途中で切れている（続きの印が立ったまま終わる）・範囲外の文字。
    assert!(decode_polyline6("_p~iF~ps|U_").is_none());
    assert!(decode_polyline6("_p~iF\u{7f}").is_none());
    assert!(decode_polyline6(" ").is_none());
    // 地球の上に無い点（緯度 100 度）を含む形。
    assert!(decode_polyline6(&encode_polyline6(&[[0.0, 100.0]])).is_none());
    // 桁あふれする差分を並べた形。
    let huge = "~".repeat(12) + "_";
    assert!(decode_polyline6(&huge.repeat(4)).is_none());
}

#[test]
fn joins_legs_without_duplicating_the_shared_point() {
    let a = [[139.0, 35.0], [139.001, 35.001]];
    let b = [[139.001, 35.001], [139.002, 35.0]];
    let body = json!({
        "trip": {
            "legs": [{ "shape": encode_polyline6(&a) }, { "shape": encode_polyline6(&b) }],
            "summary": { "length": 0.35, "time": 250.5 }
        }
    });
    let r = parse_trip(&body).unwrap();
    assert_eq!(r.coordinates.len(), 3);
    assert!((r.distance_m - 350.0).abs() < 1e-9);
    assert!((r.duration_s - 250.5).abs() < 1e-9);
    // 形が欠けた応答は受け付けない。
    assert!(
        parse_trip(&json!({ "trip": { "legs": [], "summary": { "length": 0, "time": 0 } } }))
            .is_none()
    );
    assert!(parse_trip(&json!({ "error": "x" })).is_none());
}

#[test]
fn validates_stops() {
    let stop = |lat, lng| RouteStop { lat, lng };
    let req = |stops: Vec<RouteStop>| RouteRequest {
        mode: RouteMode::Walk,
        stops,
    };
    assert!(validate(&req(vec![stop(35.0, 139.0), stop(35.1, 139.1)])).is_ok());
    assert!(validate(&req(vec![stop(35.0, 139.0)])).is_err());
    assert!(validate(&req(vec![stop(35.0, 139.0); MAX_STOPS + 1])).is_err());
    assert!(validate(&req(vec![stop(95.0, 139.0), stop(35.0, 139.0)])).is_err());
    assert!(validate(&req(vec![stop(35.0, 181.0), stop(35.0, 139.0)])).is_err());
    assert!(validate(&req(vec![stop(f64::NAN, 139.0), stop(35.0, 139.0)])).is_err());
    assert_eq!(RouteMode::Walk.costing(), "pedestrian");
    assert_eq!(RouteMode::Drive.costing(), "auto");
}
